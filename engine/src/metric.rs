// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// For every configured cell, run every metric in `METRICS`. A metric is in `METRICS` or it does
// not exist: `site/gen-data.mjs` reads each field solely from here with no fallback, so this list
// is the one place an implemented-but-unreachable measurement would be caught.
//
// The unit is a GROUP, not one metric per field: several published numbers (e.g. idle/peak/hwm/
// recovered RSS, or a peak and its concurrency) are readings off ONE window/search, and splitting
// them would re-run the measurement and risk two populations published under one name. `fields()`
// declares what a group promises; `measure()` is checked against it so a shortfall fails a test
// instead of leaving a silent hole.
//
// Every output is a `Measurement<f64>`, never a bare f64/Option: a metric that cannot measure
// returns an absence WITH A REASON.

use crate::cell::CellId;
use crate::ingress::Dialect;
use crate::measurement::{Absent, Measurement};
use crate::run::RunConfig;
use std::collections::BTreeMap;

/// Everything a metric is allowed to know about the cell it is measuring.
///
/// Deliberately small: no gateway capability declaration. `probe.rs` documents why - letting a
/// declaration reach a measurement decision let the declared cell be tried harder than others.
pub struct CellCtx<'a> {
    pub cfg: &'a RunConfig,
    pub id: &'a CellId,
    /// The ingress dialect, already parsed. A cell whose ingress does not parse never reaches a
    /// metric at all: it is recorded as untestable by the walker.
    pub dialect: Dialect,
    pub min_conc: u32,
    pub max_conc: u32,
}

/// The names a group fills, paired with what it measured.
pub type Filled = Vec<(&'static str, Measurement<f64>)>;

/// The evidence behind the headline scalars: a peak is one point out of a sweep, so the raw probed
/// points/readings travel here too, letting every published number be re-derived and charted rather
/// than trusted blind.
///
/// Empty is honest and common: a group that took no series simply returns none.
#[derive(Default)]
pub struct Series {
    /// The frontier read off `sweep` below. Structured rather than a flat field: it's a sequence,
    /// and `Filled` carries only scalars.
    pub frontier: Vec<crate::record::FrontierReading>,
    /// One entry per concurrency the throughput search actually probed, in probe order.
    pub sweep: Vec<crate::record::SweepPoint>,
    /// One entry per concurrency the SUSTAINED-throughput search probed. Kept apart from `sweep`:
    /// they are two different searches (unimodal max vs. monotone gate bisection) over the same
    /// concurrency axis, and merging their rungs would hide which point came from which search.
    pub sweep_sustained: Vec<crate::record::SweepPoint>,
    /// One entry per resident-memory reading taken across the load window.
    pub rss: Vec<crate::record::RssSample>,
    /// One entry per reading taken across the IDLE window, before any load. Kept apart from `rss`:
    /// they answer different questions (cost at rest vs. cost under work), and a reader needs to see
    /// the idle window's own shape to judge whether the baseline was itself steady.
    pub idle_rss: Vec<crate::record::RssSample>,
    /// One entry per concurrency the STREAMS-SUSTAINED gate search probed, and one per concurrency
    /// the CPU-frames/sec peak search probed. Kept apart from each other and from the two sweeps
    /// above so a rung's originating search/gate is never ambiguous.
    ///
    /// `serde_json::Value` rather than `SweepPoint` because `record.rs` types these as opaque JSON
    /// (no committed snapshot has carried one yet); `run::StreamPoint::to_json` decides the shape.
    pub sweep_streams: Vec<serde_json::Value>,
}

/// What a group produced: the fields it promised, and the evidence behind them.
#[derive(Default)]
pub struct Measured {
    pub fields: Filled,
    pub series: Series,
}

impl From<Filled> for Measured {
    /// A group that takes no series says so by returning its fields alone.
    fn from(fields: Filled) -> Self {
        Measured {
            fields,
            series: Series::default(),
        }
    }
}

/// One measurement procedure, producing one or more published numbers.
///
/// `Sync` because `METRICS` is a static slice of trait objects.
pub trait Metric: Sync {
    /// The group's name. Appears in diagnostics and in the reachability gate, never in the artifact.
    fn name(&self) -> &'static str;

    /// The artifact fields this group promises to fill, always, whether measured or absent. This is
    /// what makes "the engine silently stopped producing memory" a failing test instead of a null.
    fn fields(&self) -> &'static [&'static str];

    /// Take the measurement. Runs against a cell already known to be served.
    fn measure(&self, ctx: &CellCtx<'_>) -> Measured;
}

/// The engine's entire measurement surface. Adding a number to the board means implementing a
/// group and adding it here; removing one means deleting it from this list.
pub const METRICS: &[&dyn Metric] = &[
    &Throughput,
    &Memory,
    &Streaming,
    &AddedLatency,
    &StreamsSustained,
    &Cost,
];

/// Run every metric against one served cell.
///
/// A group that returns nothing for a field it declared gets an explicit absence rather than a
/// missing key, so the artifact's shape does not depend on which code path a metric took. A missing
/// key and a null mean different things to `site/gen-data.mjs`, and only one of them is honest.
pub fn process_cell(
    ctx: &CellCtx<'_>,
) -> (
    BTreeMap<&'static str, Measurement<f64>>,
    Series,
    BTreeMap<&'static str, f64>,
) {
    process_cell_with(ctx, METRICS)
}

/// The same loop over an EXPLICIT list, so tests can run a subset of `METRICS` instead of every
/// measurement for real (adding the streaming group to a global-reading test once turned a 0.4s
/// unit suite into 160s).
pub fn process_cell_with(
    ctx: &CellCtx<'_>,
    metrics: &[&dyn Metric],
) -> (
    BTreeMap<&'static str, Measurement<f64>>,
    Series,
    BTreeMap<&'static str, f64>,
) {
    let mut out = BTreeMap::new();
    let mut series = Series::default();
    let mut timings: BTreeMap<&'static str, f64> = BTreeMap::new();
    for m in metrics {
        // Logged before AND after each group: before, so an operator watching a live box can see
        // which group is stuck rather than going dark for minutes; after, so the group's own seconds
        // are greppable from a finished log and per-cell timing can answer "what got slower" without
        // a stopwatch rerun. The timing itself is also published in `timings` below.
        eprintln!("[phase] {} {}", ctx.id, m.name());
        let started = std::time::Instant::now();
        let produced = m.measure(ctx);
        let took = started.elapsed();
        eprintln!(
            "[phase] {} {} took {:.1}s",
            ctx.id,
            m.name(),
            took.as_secs_f64()
        );
        timings.insert(m.name(), took.as_secs_f64());
        // Series ACCUMULATE across groups rather than overwrite: a later group returning none for a
        // field must not erase an earlier group's evidence for it (e.g. throughput fills the sweep,
        // memory fills the RSS readings).
        if !produced.series.sweep.is_empty() {
            series.sweep = produced.series.sweep;
        }
        if !produced.series.frontier.is_empty() {
            series.frontier = produced.series.frontier;
        }
        if !produced.series.sweep_sustained.is_empty() {
            series.sweep_sustained = produced.series.sweep_sustained;
        }
        if !produced.series.rss.is_empty() {
            series.rss = produced.series.rss;
        }
        if !produced.series.sweep_streams.is_empty() {
            series.sweep_streams = produced.series.sweep_streams;
        }
        // This accumulator is a hand-written chain, one clause per field - it once had no clause for
        // `idle_rss`, so the idle window was measured but silently dropped on every cell. An
        // accumulator that forgets a field looks exactly like a group that produced none; see the
        // regression test `no_series_field_is_dropped_by_the_accumulator` below.
        if !produced.series.idle_rss.is_empty() {
            series.idle_rss = produced.series.idle_rss;
        }
        let filled: BTreeMap<&'static str, Measurement<f64>> =
            produced.fields.into_iter().collect();
        for field in m.fields() {
            let value = filled.get(field).cloned().unwrap_or_else(|| {
                Measurement::absent_because(
                    Absent::NotMeasured,
                    format!(
                        "the {} group declares {field} but returned no value for it",
                        m.name()
                    ),
                )
            });
            out.insert(*field, value);
        }
    }
    (out, series, timings)
}

// ── the groups ────────────────────────────────────────────────────────────────────────────────────

/// Throughput: the gateway's proxied requests per second at its peak, and the concurrency that peak
/// happened at. One search, two numbers - which is the whole reason a group is the unit.
pub struct Throughput;

impl Metric for Throughput {
    fn name(&self) -> &'static str {
        "throughput"
    }

    /// No scalar fields: this group's whole output is the FRONTIER, a sequence, so it travels on
    /// `series` beside the sweep it's read from rather than in `Filled`. It replaced five scalars
    /// (`rps_max_proxy`/`conc_at_peak`/`rps_sustained_20ms`/...) that each collapsed the same
    /// tradeoff curve to one point chosen by a constant. See `frontier.rs`.
    fn fields(&self) -> &'static [&'static str] {
        &[]
    }

    fn measure(&self, ctx: &CellCtx<'_>) -> Measured {
        let perf = crate::run::sweep_cell(ctx.cfg, ctx.id, ctx.min_conc, ctx.max_conc);
        // The frontier is read off the rungs this sweep already probed - no extra measurement. See
        // `frontier.rs`.
        //
        // A rung with no window reading contributes nothing, not a zero: `ok`/`fail` of 0/0 fails
        // `served_cleanly`, disqualifying the rung from every bound rather than counting it clean.
        let rungs: Vec<crate::frontier::Rung> = perf
            .points
            .iter()
            .map(|pt| crate::frontier::Rung {
                concurrency: pt.concurrency,
                rps: pt.value,
                p99_us: pt.reading.and_then(|r| r.p99_us),
                ok: pt.reading.map(|r| r.ok).unwrap_or(0),
                fail: pt.reading.map(|r| r.fail).unwrap_or(0),
            })
            .collect();
        let frontier: Vec<crate::record::FrontierReading> = crate::frontier::P99_BOUNDS_US
            .iter()
            .map(|b| Some(*b))
            .chain(std::iter::once(None))
            .map(|bound| match crate::frontier::read_at(&rungs, bound) {
                Some(r) => crate::record::FrontierReading {
                    p99_bound_us: bound.map(|b| b as i64),
                    rps: Measurement::Measured(r.rps),
                    concurrency: Measurement::Measured(i64::from(r.concurrency)),
                    // The tail the winning rung actually produced. Absent only when the rung carried
                    // no latency reading (possible only for the unbounded reading).
                    p99_us: match r.p99_us {
                        Some(p) => Measurement::Measured(p as i64),
                        None => Measurement::absent_because(
                            Absent::NotMeasured,
                            "no window behind this rung reported a tail latency".to_string(),
                        ),
                    },
                    first_disqualified_conc: match r.first_disqualified_conc {
                        Some(c) => Measurement::Measured(i64::from(c)),
                        // Not a hole: the sweep ran out of range while this bound still held - see
                        // `lower_bound` beside it.
                        None => Measurement::absent_because(
                            Absent::SearchExhausted,
                            "every rung above this one also held this bound, so the sweep ran out of \
                             range rather than finding the boundary"
                                .to_string(),
                        ),
                    },
                    lower_bound: r.is_lower_bound(),
                },
                // A bound nothing qualified for is still a published column, carrying WHY - dropping
                // it would make the frontier's length vary per cell.
                None => {
                    let absent = crate::frontier::absence_for(&rungs, bound);
                    // Generic over the measurement's type: `rps` is f64, its siblings are counts, and
                    // one absence has to be spellable for both.
                    let carry = |()| -> Measurement<f64> {
                        match (absent.reason().cloned(), absent.detail()) {
                            (Some(rr), Some(d)) => Measurement::absent_because(rr, d),
                            (Some(rr), None) => Measurement::absent(rr),
                            (None, _) => Measurement::absent(Absent::NotMeasured),
                        }
                    };
                    let carry_i = |()| -> Measurement<i64> {
                        match (absent.reason().cloned(), absent.detail()) {
                            (Some(rr), Some(d)) => Measurement::absent_because(rr, d),
                            (Some(rr), None) => Measurement::absent(rr),
                            (None, _) => Measurement::absent(Absent::NotMeasured),
                        }
                    };
                    crate::record::FrontierReading {
                        p99_bound_us: bound.map(|b| b as i64),
                        rps: carry(()),
                        concurrency: carry_i(()),
                        p99_us: carry_i(()),
                        first_disqualified_conc: carry_i(()),
                        lower_bound: false,
                    }
                }
            })
            .collect();
        // The sweep travels with the peak: each probed rung becomes a published point, so a reader
        // can see the shape the search walked and re-derive the maximum rather than trust it.
        //
        // `p99_us` and `fail` come from the window itself, not a fabricated 0 when absent: "measured
        // no failures" and "nothing was measured" are different facts.
        let sweep = perf
            .points
            .iter()
            .map(|pt| crate::record::SweepPoint {
                conc: i64::from(pt.concurrency),
                // Absent, never a fabricated 0, when no window reported: "completed nothing" and
                // "nothing was measured" are different facts.
                ok: match pt.reading {
                    Some(r) => Measurement::Measured(r.ok as i64),
                    None => Measurement::absent(Absent::NotMeasured),
                },
                rps: Measurement::Measured(pt.value),
                p99_us: match pt.reading.and_then(|r| r.p99_us) {
                    Some(v) => Measurement::Measured(v as i64),
                    None => Measurement::absent(Absent::NotMeasured),
                },
                fail: match pt.reading {
                    Some(r) => Measurement::Measured(r.fail as i64),
                    None => Measurement::absent(Absent::NotMeasured),
                },
            })
            .collect();
        Measured {
            fields: Vec::new(),
            series: Series {
                frontier,
                sweep,
                ..Series::default()
            },
        }
    }
}

/// The concurrency the memory window runs at.
///
/// A constant, not the cell's own peak: memory is compared ACROSS gateways, so every window must be
/// the same load, or ranking would compare thirteen different workloads as if they were one. Also
/// not derived from core count - a comparison recipe that moves with the hardware would make two
/// boxes' numbers incomparable.
pub const MEMORY_WINDOW_CONCURRENCY: u32 = 32;

/// How often the resident-memory sampler reads the tree during the window.
const MEMORY_SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Load runs until memory stops moving, not for a fixed time: a fixed window reports wherever the
/// gateway happened to be at cutoff, so gateways that settle at different speeds would be compared
/// at different points on their own curves. Running until the trailing window is flat measures the
/// same thing on every entrant - where it actually levels off.
///
/// Three-way verdict: `Steady` is a settled number, `NotSteady` carries the growth rate (a gateway
/// that never levels off is published with how fast it climbed), `Undecidable` means too few
/// samples - deliberately not the same claim as "it moved".
///
/// The cap is not a fallback but the reason a leak terminates: hitting it is a result (`NotSteady`),
/// never an error.
pub const MEMORY_PLATEAU_WINDOW_S: f64 = 60.0;
pub const MEMORY_LOAD_S: u64 = 300;

/// After load stops, sampling continues for this long and the trailing reading publishes as
/// recovered: peak answers what a gateway costs while working, not whether it gives memory back
/// afterward, and a peak alone can't tell a gateway that returns to 8 MiB apart from one that stays
/// at 120. Same duration as the settle window so the two halves of the curve are comparable.
pub const MEMORY_RECOVERY_S: u64 = 60;
/// The trailing slice of the recovery window that `recovered_rss_mib` is the MEDIAN of - not the
/// whole 60s, since the first half still holds the allocator's descent from peak and would average
/// neither level meaningfully. Named as its own constant (previously an inline `/ 2.0` while the
/// published `recovery_window_s` claimed 60 and the chart subtitle disagreed with what was measured)
/// so the artifact discloses the slice the number actually came from.
pub const MEMORY_RECOVERY_MEDIAN_S: u64 = MEMORY_RECOVERY_S / 2;
/// How long the process is watched BEFORE any load - a window, not a single instantaneous sample,
/// because a still-settling process reads momentarily low (overstating growth figures derived from
/// it) and a gateway leaking with nothing asked of it is invisible with only one point. Same
/// duration as the recovery window so idle and `recovered_rss_mib` are directly comparable.
pub const MEMORY_IDLE_S: u64 = 60;
/// Percent the trailing window's two halves may differ by, and percent spread within it, before the
/// window counts as still moving. The values the shell suite used, kept so the two agree.
pub const MEMORY_TREND_PCT: f64 = 1.0;
pub const MEMORY_RANGE_PCT: f64 = 2.0;

/// Whether a steady verdict may be BELIEVED, given how the series grew since the last window.
///
/// A dead sampler looks exactly like a settled gateway: if the sampler thread dies mid-window, the
/// series stops growing, and a frozen tail has zero drift/spread - the textbook definition of
/// steady. So the loop would publish "settled" plus a peak that is really "whatever was captured
/// before the sampler died". The discriminator is growth: a live sampler keeps adding samples
/// between windows even when the gateway itself is flat; zero new samples means no measurement, not
/// a calm gateway.
fn steady_is_believable(samples_before: usize, samples_now: usize) -> bool {
    samples_now > samples_before
}

/// Has the series existed long enough for a steadiness verdict to MEAN anything?
///
/// `stats::window` selects by timestamp, so a series only six seconds long yields a "sixty second
/// window" holding six seconds of data; `plateau_check`'s own `n < 4` guard doesn't catch this since
/// this sampler takes ten readings/sec. Those six seconds would then be judged against thresholds
/// chosen for a full minute, and a gateway climbing slowly barely drifts that fast - so the first
/// load window could come back `Steady` and understate the peak.
///
/// Kept OUT of `plateau_check` (a general statistic with its own callers/tests) because the decision
/// to STOP is specific to this loop. Kept separate from `steady_is_believable`: a series that
/// stopped growing means the sampler is gone (abort), a series that's merely too short means keep
/// measuring (do not abort) - folding them together would abort a healthy cell on its first window.
fn window_is_long_enough(span_s: f64) -> bool {
    span_s >= MEMORY_PLATEAU_WINDOW_S
}

/// The unsettled SHAPE as a number, since the metric surface only carries `f64`.
///
/// 1 climbing, 0 oscillating, -1 falling. Signed on purpose: the sign IS the direction, so "greater
/// than zero is bad" is correct without a lookup table.
fn shape_code(shape: crate::stats::Shape) -> f64 {
    match shape {
        crate::stats::Shape::Climbing => 1.0,
        crate::stats::Shape::Oscillating => 0.0,
        crate::stats::Shape::Falling => -1.0,
    }
}

/// Memory: what the gateway's process tree costs at rest and under load.
///
/// Four readings of ONE window, which is why this is a group: taking idle from one window and peak
/// from another would publish two populations side by side for the same gateway.
///
/// `peak` is sampled, so it can miss a spike between polls; `hwm` is the kernel's own high-water
/// mark, updated on every charge, so it cannot. Both are published since they answer different
/// questions and disagreeing is informative.
pub struct Memory;

impl Metric for Memory {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn fields(&self) -> &'static [&'static str] {
        &[
            "memory_idle_mib",
            // Whether the process was still or growing with nothing asked of it, and the rate if it
            // grew - a leak at idle is the most damning result and a single-sample idle couldn't see it.
            "memory_idle_static",
            "memory_idle_growth_rate_mib_per_min",
            // How each window failed to settle, when it did. See `shape_code`.
            "memory_idle_shape",
            "memory_shape",
            "memory_peak_mib",
            "memory_hwm_mib",
            "memory_recovered_mib",
            "memory_growth_rate_mib_per_min",
            "memory_load_s",
            "memory_plateaued",
            "memory_time_to_plateau_s",
        ]
    }

    fn measure(&self, ctx: &CellCtx<'_>) -> Measured {
        // The tree to measure comes from the ONE declared identity, the same one the launcher's
        // --name and the stop path use.
        let mut pid = match crate::rss::root_pid(&ctx.cfg.runtime).copied() {
            Some(p) => p,
            None => {
                // No process to measure. Every field carries the SAME reason - one cause, one
                // explanation - rather than independently-worded absences for one fact.
                let why = crate::rss::root_pid(&ctx.cfg.runtime);
                let reason = why.reason().cloned().unwrap_or(Absent::NotMeasured);
                let detail = why
                    .detail()
                    .unwrap_or("the gateway's process tree could not be found")
                    .to_string();
                let fields: Filled = self
                    .fields()
                    .iter()
                    .map(|f| {
                        (
                            *f,
                            Measurement::absent_because(reason.clone(), detail.clone()),
                        )
                    })
                    .collect();
                // No process, so no window ran and there is no series to carry.
                return fields.into();
            }
        };

        // ── PUT IT BACK AT REST FIRST ────────────────────────────────────────────────────────────
        //
        // METRICS runs Throughput before Memory on the same process, so reading `idle` here without
        // restarting first would read post-load RSS under the name "idle" - allocators don't return
        // memory promptly, and the reading would also be order-dependent across cells. So the process
        // is restarted and only then read; if the harness doesn't own the gateway's lifetime it can't
        // be returned to rest, and idle publishes ABSENT with that reason instead of a polluted number.
        // Filled by the idle window below and published beside the load series, so the site can draw
        // the two windows as two curves on one scale rather than collapsing idle to a single number.
        let mut idle_series: Vec<crate::stats::Sample> = Vec::new();
        let idle = match &ctx.cfg.relaunch {
            None => Measurement::absent_because(
                Absent::NotMeasured,
                "the harness does not own this gateway's lifetime, so it could not be returned to \
                 rest before the reading; an idle taken after the throughput sweep would be \
                 post-load RSS under another name"
                    .to_string(),
            ),
            Some(spec) => match crate::run::restart_to_rest(
                spec,
                &ctx.cfg.relaunch_launcher,
                &ctx.cfg.relaunch_commands,
            ) {
                // A failed restart aborts the WHOLE group, not just idle: falling through to the
                // sampler/load window would measure a gateway in an unknown state, with `pid` still
                // pointing at the pre-restart tree, so every number would be the rig's own failure
                // wearing the gateway's name.
                Err(e) => {
                    let f: Filled = self
                        .fields()
                        .iter()
                        .map(|x| {
                            (
                                *x,
                                Measurement::absent_because(
                                    Absent::HarnessError,
                                    format!(
                                        "the gateway could not be restarted to rest, so the memory \
                                         window did not run: {e}"
                                    ),
                                ),
                            )
                        })
                        .collect();
                    return f.into();
                }
                // Re-resolve the pid: a restart gives the tree a NEW root, and reading the old one
                // would measure a process that no longer exists.
                Ok(()) => match crate::rss::root_pid(&ctx.cfg.runtime).copied() {
                    Some(fresh) => {
                        pid = fresh;
                        // Sampled at the same interval as the load window, so `idle_series` is the
                        // same shape of evidence as `rss_series`.
                        let idle_start = std::time::Instant::now();
                        while idle_start.elapsed().as_secs() < MEMORY_IDLE_S {
                            if let Some(v) = crate::rss::rss_tree_mib(fresh).copied() {
                                idle_series.push(crate::stats::Sample {
                                    t_s: idle_start.elapsed().as_secs_f64(),
                                    mib: v,
                                });
                            }
                            std::thread::sleep(MEMORY_SAMPLE_INTERVAL);
                        }
                        // The MEDIAN of the window, not first/last reading, so one allocator spike
                        // cannot set the baseline every other memory figure is measured against.
                        let vals: Vec<f64> = idle_series.iter().map(|s| s.mib).collect();
                        if vals.is_empty() {
                            Measurement::absent_because(
                                Absent::NotMeasured,
                                format!(
                                    "the {MEMORY_IDLE_S}s idle window produced no readable sample of the process tree"
                                ),
                            )
                        } else {
                            crate::stats::median(&vals)
                        }
                    }
                    None => Measurement::absent_because(
                        Absent::NotMeasured,
                        "the gateway restarted but its process tree could not be found afterwards"
                            .to_string(),
                    ),
                },
            },
        };

        // Sample the tree while a window of load runs against it. The sampler is a plain thread
        // rather than a timer: it stops when the window's child exits, so a slow window is sampled
        // for as long as it actually ran instead of for as long as it was expected to.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let peak_seen = std::sync::Arc::new(std::sync::Mutex::new(f64::NEG_INFINITY));
        // Keep the readings, not just their maximum: whether memory climbed and plateaued or spiked
        // once is the difference between a leak and a burst, invisible from a single scalar.
        let series = std::sync::Arc::new(std::sync::Mutex::new(Vec::<crate::stats::Sample>::new()));
        let sampler = {
            let stop = std::sync::Arc::clone(&stop);
            let peak_seen = std::sync::Arc::clone(&peak_seen);
            let series = std::sync::Arc::clone(&series);
            let started = std::time::Instant::now();
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Some(v) = crate::rss::rss_tree_mib(pid).copied() {
                        if let Ok(mut p) = peak_seen.lock() {
                            *p = p.max(v);
                        }
                        if let Ok(mut s) = series.lock() {
                            // Sub-second precision internally: the plateau test compares two halves
                            // of a trailing window, and at ten readings/sec a truncated stamp would
                            // bucket them together and make the trend meaningless.
                            s.push(crate::stats::Sample::new(
                                started.elapsed().as_secs_f64(),
                                v,
                            ));
                        }
                    }
                    std::thread::sleep(MEMORY_SAMPLE_INTERVAL);
                }
            })
        };

        let path = crate::run::path_for(ctx.cfg, ctx.dialect, &ctx.id.egress);
        // The cell's own model, never the bare declared one: most gateways route on the model name,
        // so a fixed model would drive this window at a different upstream than the cell it's
        // published under (`run::model_for`'s own contract).
        let body = ctx
            .dialect
            .body(&crate::run::model_for(ctx.cfg, &ctx.id.egress));
        // The same headers the probe authenticated this cell with - the wrong credential would
        // measure a process serving 401s, a different workload from what every other window compares.
        let headers = crate::run::headers_for(ctx.cfg, ctx.dialect, &ctx.id.egress);

        // ── LOAD UNTIL IT STOPS MOVING ───────────────────────────────────────────────────────────
        //
        // Repeated windows rather than one long one, since the plateau test needs to be asked
        // between windows. The loop ends when the trailing minute is flat, or at the cap; hitting the
        // cap is a RESULT (never settled), not a failure.
        let load_started = std::time::Instant::now();
        let mut ran = None;
        let mut verdict =
            crate::stats::Verdict::Undecidable(crate::stats::Undecidable::TooFewReadings {
                got: 0,
                need: 4,
            });
        let mut settled_at = None;
        // Samples the series held when last looked at, so a series that stops growing is
        // distinguishable from a gateway that stopped moving.
        let mut samples_before = 0usize;
        let mut sampler_died = false;
        loop {
            let w =
                crate::run::load_window(ctx.cfg, &path, &body, &headers, MEMORY_WINDOW_CONCURRENCY);
            // A window that produced nothing means the load never ran. Stop rather than spinning on
            // a gateway that is not answering; the peak below is then an honest absence.
            if w.is_none() {
                break;
            }
            ran = w;
            let taken: Vec<crate::stats::Sample> =
                series.lock().map(|s| s.clone()).unwrap_or_default();
            verdict = crate::stats::plateau_check(
                &taken,
                MEMORY_PLATEAU_WINDOW_S,
                MEMORY_TREND_PCT,
                MEMORY_RANGE_PCT,
            );
            // A steady verdict off a series that did not grow is the sampler's death, not the
            // gateway's calm. See `steady_is_believable`.
            let span = taken.last().map(|s| s.t_s).unwrap_or(0.0)
                - taken.first().map(|s| s.t_s).unwrap_or(0.0);
            let grew = steady_is_believable(samples_before, taken.len());
            samples_before = taken.len();
            // Too soon to believe it - keep loading. See `window_is_long_enough`.
            if verdict.is_steady() && !window_is_long_enough(span) {
                verdict =
                    crate::stats::Verdict::Undecidable(crate::stats::Undecidable::WindowTooShort);
            }
            if verdict.is_steady() && !grew {
                eprintln!(
                    "memory: the RSS series stopped growing at {} samples while the load window was \
                     still running - the sampler thread is gone, so 'steady' here is the absence of \
                     measurement rather than a reading of the gateway",
                    taken.len()
                );
                sampler_died = true;
                break;
            }
            // THE VERDICT IS RECORDED, NOT ACTED ON: when the trailing window first reads flat this
            // notes WHEN and keeps loading, rather than `break`ing here. Breaking early would let a
            // threshold decide when to stop measuring (and thus decide the number) - a gateway that
            // looked flat early would never be asked the question a busy-looking one was, yet their
            // peaks would be ranked against each other. So every cell now runs the full
            // `MEMORY_LOAD_S`, buying a peak that means the same thing on every row.
            if verdict.is_steady() && settled_at.is_none() {
                settled_at = Some(load_started.elapsed().as_secs() as i64);
            }
            if load_started.elapsed().as_secs() >= MEMORY_LOAD_S {
                break;
            }
        }
        let load_s = load_started.elapsed().as_secs() as i64;

        // The verdict, taken over the FULL load: the loop's last `plateau_check` ran mid-load only to
        // note `settled_at`, but the published verdict must describe the whole measured window. A
        // cell that settled at 90s and then climbed for 200 more is NOT steady.
        if !sampler_died {
            let full: Vec<crate::stats::Sample> =
                series.lock().map(|s| s.clone()).unwrap_or_default();
            let span = full.last().map(|x| x.t_s).unwrap_or(0.0)
                - full.first().map(|x| x.t_s).unwrap_or(0.0);
            verdict = crate::stats::plateau_check(
                &full,
                MEMORY_PLATEAU_WINDOW_S,
                MEMORY_TREND_PCT,
                MEMORY_RANGE_PCT,
            );
            if verdict.is_steady() && !window_is_long_enough(span) {
                verdict =
                    crate::stats::Verdict::Undecidable(crate::stats::Undecidable::WindowTooShort);
            }
        }

        // The kernel's high-water mark is read BEFORE the recovery window, while it still describes
        // the loaded process. It survives the load ending, but reading it here keeps it beside the
        // peak it belongs to.
        let hwm = crate::rss::hwm_tree_mib(pid);

        // ── THEN WATCH IT WITH THE LOAD GONE ─────────────────────────────────────────────────────
        //
        // The sampler is still running, so this is simply a minute of quiet appended to the same
        // series, showing whether the gateway hands memory back - which a peak cannot say.
        std::thread::sleep(std::time::Duration::from_secs(MEMORY_RECOVERY_S));

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        // The join result is the only evidence the sampler panicked (previously discarded with
        // `let _`, so a mid-window sampler death left a plausible, self-consistent result with
        // nothing saying the readings had stopped).
        if sampler.join().is_err() {
            sampler_died = true;
            eprintln!(
                "memory: the RSS sampler thread PANICKED during this cell's load window, so the \
                 readings stop at whatever it captured before dying and no plateau verdict taken \
                 from them describes this gateway"
            );
        }

        // A dead sampler aborts the WHOLE group, same reason as a failed restart above: the peak/
        // series/verdict all describe our instrument's failure, not the gateway, and publishing any
        // of it would be worse than nothing since it looks self-consistent.
        if sampler_died {
            let f: Filled = self
                .fields()
                .iter()
                .map(|x| {
                    (
                        *x,
                        Measurement::absent_because(
                            Absent::HarnessError,
                            "the RSS sampler stopped during the load window, so every memory reading \
                             for this cell ends where our instrument failed rather than where the \
                             gateway did"
                                .to_string(),
                        ),
                    )
                })
                .collect();
            return f.into();
        }

        let peak = match (ran, peak_seen.lock().ok().map(|p| *p)) {
            // A window that never ran means the peak was never put under load - absence, not the
            // idle reading under a false name.
            (None, _) => Measurement::absent_because(
                Absent::NotMeasured,
                "the load window did not run, so no memory reading was taken under load".to_string(),
            ),
            (Some(_), Some(v)) if v.is_finite() => Measurement::Measured(v),
            (Some(_), _) => Measurement::absent_because(
                Absent::NotMeasured,
                format!("the load window ran but no /proc reading of the tree rooted at pid {pid} succeeded"),
            ),
        };

        // A poisoned lock means the sampler thread panicked - a lost series, not a reason to lose
        // the scalars beside it.
        let taken: Vec<crate::stats::Sample> = series.lock().map(|s| s.clone()).unwrap_or_default();

        // Recovered: where the curve ends after load has been gone for a minute, taken from the
        // trailing recovery window (not the single last reading) so one sample can't set it.
        let recovered = {
            let cut = taken.last().map(|s| s.t_s).unwrap_or(0.0) - MEMORY_RECOVERY_MEDIAN_S as f64;
            let tail: Vec<f64> = taken
                .iter()
                .filter(|s| s.t_s >= cut)
                .map(|s| s.mib)
                .collect();
            crate::stats::median(&tail)
        };
        // The plateau verdict is published, carrying the climb rate ("never settled" alone would not
        // distinguish it from "we could not tell"). The shape rides with it too: "never settled"
        // covers both unbounded climbing and oscillation around a level, and only the first is a
        // leak - conflating them would brand a working garbage collector as a defect.
        let mut memory_shape = Measurement::absent_because(
            Absent::NotMeasured,
            "a settled window has no unsettled shape to describe".to_string(),
        );
        let (plateaued, growth) = match &verdict {
            // Steady publishes the measured slope, never a substituted 0.0: a window drifting 0.9%
            // (inside `MEMORY_TREND_PCT`, hence steady) still has a real fitted rate a reader could
            // use to spot a slow leak. `plateaued` is the verdict, this is the measurement - the
            // artifact publishes both, neither derived from the other.
            crate::stats::Verdict::Steady {
                growth_rate_mib_per_min,
            } => (Some(true), growth_rate_mib_per_min.clone()),
            crate::stats::Verdict::NotSteady {
                growth_rate_mib_per_min,
                shape,
            } => {
                memory_shape = Measurement::Measured(shape_code(*shape));
                (Some(false), growth_rate_mib_per_min.clone())
            }
            // The cause decides the variant and the wording, rather than one hardcoded reason for
            // every kind of undecidable window (previously filed a harness malfunction as a plain
            // coverage gap).
            crate::stats::Verdict::Undecidable(cause) => {
                // `memory_shape`'s default ("a settled window has no unsettled shape") is wrong here:
                // the verdict's whole content is that we could NOT tell whether it settled, so the
                // shape must say that too rather than implicitly claiming it settled.
                memory_shape =
                    Measurement::absent_because(cause.absent_kind(), cause.detail("settle window"));
                (
                    None,
                    Measurement::absent_because(cause.absent_kind(), cause.detail("settle window")),
                )
            }
        };
        // The SAME plateau test the load window uses, pointed at the idle window, so "still" means
        // the same thing on both halves of the curve.
        //
        // No idle window at all is NOT the same as a thin one: `idle_series` stays empty whenever no
        // idle window was opened (any path but the harness-owns-lifetime + fresh-tree one), so the
        // defaults below must say "never ran" rather than implying a window that ran and produced
        // too little.
        let no_idle_window = idle_series.is_empty();
        let never_ran =
            "no idle window was opened for this cell, so nothing was sampled - which is \
                         not the same as sampling and finding too little"
                .to_string();
        let mut idle_shape = Measurement::absent_because(
            Absent::NotMeasured,
            if no_idle_window {
                never_ran.clone()
            } else {
                "a settled idle window has no unsettled shape to describe".to_string()
            },
        );
        let (idle_static, idle_growth) = if idle_series.len() < 2 {
            let why = if no_idle_window {
                never_ran.clone()
            } else {
                format!(
                    "the {MEMORY_IDLE_S}s idle window produced too few readings to say whether memory moved"
                )
            };
            (
                Measurement::absent_because(Absent::NotMeasured, why.clone()),
                Measurement::absent_because(Absent::NotMeasured, why),
            )
        } else {
            // The verdict carries the rate, so "it moved" can never publish without saying how fast.
            match crate::stats::plateau_check(
                &idle_series,
                MEMORY_IDLE_S as f64,
                MEMORY_TREND_PCT,
                MEMORY_RANGE_PCT,
            ) {
                // Same fix as the load window above: a steady idle window still has a fitted slope
                // worth publishing rather than a substituted zero.
                crate::stats::Verdict::Steady {
                    growth_rate_mib_per_min,
                } => (Measurement::Measured(1.0), growth_rate_mib_per_min),
                crate::stats::Verdict::NotSteady {
                    growth_rate_mib_per_min,
                    shape,
                } => {
                    idle_shape = Measurement::Measured(shape_code(shape));
                    (Measurement::Measured(0.0), growth_rate_mib_per_min)
                }
                crate::stats::Verdict::Undecidable(cause) => {
                    let why = cause.detail(&format!("{MEMORY_IDLE_S}s idle window"));
                    (
                        Measurement::absent_because(cause.absent_kind(), why.clone()),
                        Measurement::absent_because(cause.absent_kind(), why),
                    )
                }
            }
        };
        let idle_rss_samples: Vec<crate::record::RssSample> = idle_series
            .iter()
            .map(|s| crate::record::RssSample {
                t_s: s.t_s as i64,
                rss_mib: Measurement::Measured(s.mib),
            })
            .collect();
        let rss: Vec<crate::record::RssSample> = taken
            .iter()
            .map(|s| crate::record::RssSample {
                t_s: s.t_s as i64,
                rss_mib: Measurement::Measured(s.mib),
            })
            .collect();
        Measured {
            fields: vec![
                ("memory_idle_mib", idle),
                ("memory_idle_static", idle_static),
                ("memory_idle_growth_rate_mib_per_min", idle_growth),
                ("memory_idle_shape", idle_shape),
                ("memory_shape", memory_shape),
                ("memory_peak_mib", peak),
                ("memory_hwm_mib", hwm),
                ("memory_recovered_mib", recovered),
                ("memory_growth_rate_mib_per_min", growth),
                (
                    "memory_load_s",
                    Measurement::Measured(load_s as f64),
                ),
                (
                    "memory_plateaued",
                    match plateaued {
                        Some(v) => Measurement::Measured(if v { 1.0 } else { 0.0 }),
                        None => Measurement::absent(Absent::NotMeasured),
                    },
                ),
                (
                    "memory_time_to_plateau_s",
                    match settled_at {
                        Some(t) => Measurement::Measured(t as f64),
                        None => Measurement::absent_because(
                            Absent::NotMeasured,
                            // Reason strings ride into the board's tooltips verbatim, and this one
                            // names the SHAPE: "no steady state" alone reads the same for a gateway
                            // still climbing as for one still releasing memory, which are opposites.
                            no_plateau_detail(&verdict),
                        ),
                    },
                ),
            ],
            series: Series { rss, idle_rss: idle_rss_samples, ..Series::default() },
        }
    }
}

/// How long a streaming probe waits, and how many frames it reads.
///
/// Public because the CONCURRENT stream windows (`run::stream_window`, behind the two groups below)
/// must read a stream exactly the way the c=1 probe here does - two readers with two budgets would
/// measure two different stream lengths and publish the difference as a gateway property.
pub const STREAM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
pub const STREAM_FRAME_BUDGET: usize = 64;

/// The hard ceiling on TOTAL SSE events a delivery-budgeted read will spend (`http::SseBudget`).
///
/// A read budgeted in content frames stops when the tokens arrive, so a peer emitting nothing but
/// `ping`s needs a separate bound or it's only limited by `STREAM_TIMEOUT` - too slow to search at
/// high concurrency. 4x the frame budget gives room for three framing events per token, generous
/// past anything a real protocol does, since the ceiling exists to stop a pathological peer, not to
/// judge a gateway's framing style.
pub const STREAM_EVENT_CEILING: usize = 4 * STREAM_FRAME_BUDGET;

/// How many single-token streams the TTFT distribution is taken over, per leg. A percentile needs
/// samples - one stream yields exactly one time-to-first-token. 100 is the smallest sample where a
/// 99th percentile is a real order statistic rather than a restatement of the max, and it's
/// affordable since a TTFT sample reads ONE frame and stops (milliseconds, not the ~1.3s a full
/// paced stream takes).
pub const STREAM_TTFT_SAMPLES: usize = 100;

/// Streaming: what the gateway ADDS to a stream, rather than what the stream costs.
///
/// Every number here is a difference. The same stream is taken through the gateway and again
/// straight to the mock, and what is published is the gap between them, because the mock's own time
/// to first token is a property of the rig and would otherwise be charged to whichever gateway
/// happened to be measured on a slow box.
///
/// A dialect the MOCK cannot stream is a rig limit, not a gateway failure: `Dialect::streams_natively`
/// records which dialects the mock answers with real SSE frames, and a `false` there must be reported
/// as the rig being unable to pose the question rather than the gateway failing to stream.
pub struct Streaming;

impl Metric for Streaming {
    fn name(&self) -> &'static str {
        "streaming"
    }

    /// The last two are surface-only, feeding `CellStream.stream_c1_note` rather than a published
    /// number: how many frames each leg produced from the one stream per leg this group takes.
    fn fields(&self) -> &'static [&'static str] {
        &[
            "added_ttft_p50_us",
            "added_ttft_p99_us",
            "added_gap_p50_us",
            "added_gap_p99_us",
            "gateway_c1_frames",
            "direct_c1_frames",
            // How many TTFT probes survived, per leg. A failed probe is dropped inside `filter_map`,
            // so without this a p99 over three lucky samples would publish identically to one over a
            // hundred, with no way for a reader to tell the weight behind it.
            "ttft_gw_samples",
            "ttft_direct_samples",
        ]
    }

    fn measure(&self, ctx: &CellCtx<'_>) -> Measured {
        // Streaming takes no series of its own; `into()` wraps its fields with an empty one.
        let all = |m: Measurement<f64>| -> Measured {
            let f: Filled = self.fields().iter().map(|x| (*x, m.clone())).collect();
            f.into()
        };

        // The rig, not the gateway, decides whether this question can be asked at all - and it is
        // asked of BOTH ends, since the frames come from the egress upstream.
        if let Some(side) = stream_blocked_by(ctx) {
            return all(Measurement::absent_because(
                Absent::Untestable,
                format!(
                    "the mock does not answer {side} with a native event stream, so the rig cannot pose the streaming question here"
                ),
            ));
        }

        // The header shape is the dialect's, not a hardcoded bearer: anthropic sends `x-api-key` plus
        // a version header, gemini sends `x-goog-api-key`, so a fixed `authorization: Bearer` header
        // would 401 those legs and read as "does not stream". The gateway leg additionally carries
        // whatever routing headers the manifest needs to select this egress column; the direct leg
        // carries only the dialect's own auth, since routing headers mean nothing to the mock.
        let gw_headers = crate::run::headers_for(ctx.cfg, ctx.dialect, &ctx.id.egress);
        let direct_headers = ctx.dialect.auth_headers(&ctx.cfg.auth);
        // The cell's own model (`run::model_for`): a fixed model would stream against the wrong
        // upstream on any model-routed gateway's translation cell.
        let model = crate::run::model_for(ctx.cfg, &ctx.id.egress);
        let body = ctx.dialect.stream_body(&model);

        let through_gateway = crate::http::post_json_sse(
            ctx.cfg.gateway_addr,
            &crate::run::path_for(ctx.cfg, ctx.dialect, &ctx.id.egress),
            body.as_bytes(),
            &gw_headers,
            STREAM_TIMEOUT,
            STREAM_FRAME_BUDGET,
            Some(ctx.dialect),
        );
        let direct = crate::http::post_json_sse(
            ctx.cfg.mock_addr,
            &ctx.dialect.mock_direct_path(&model),
            body.as_bytes(),
            &direct_headers,
            STREAM_TIMEOUT,
            STREAM_FRAME_BUDGET,
            Some(ctx.dialect),
        );

        // A leg that produced no frame has no time to first token. Subtracting against a missing
        // reference would publish the gateway's own latency as its ADDED latency, which reads as the
        // gateway being slower than it is.
        let (Some(&gw_ttft), Some(&direct_ttft)) = (
            through_gateway.frame_offsets_us.first(),
            direct.frame_offsets_us.first(),
        ) else {
            let which = if through_gateway.frame_offsets_us.is_empty() {
                "the gateway"
            } else {
                "the mock directly"
            };
            return all(Measurement::absent_because(
                Absent::NotMeasured,
                format!("no stream frame arrived from {which}, so there is nothing to difference"),
            ));
        };

        // The two legs above are read only to PROVE a stream arrives on each: the published TTFT
        // figures come from the sample set below, since one observation cannot carry a percentile -
        // a p50 from one stream beside a p99 from a hundred is two populations wearing one name.
        let _ = (gw_ttft, direct_ttft);

        // A TTFT distribution, not a single sample: one stream yields exactly one
        // time-to-first-token, so a real p99 needs `STREAM_TTFT_SAMPLES` streams.
        //
        // Cheap because it does not need the whole stream: `post_json_sse` with a frame budget of ONE
        // returns on the first EVENT (`SseBudget::Events(1)`), not the first content token - a dialect
        // that opens with scaffolding (openai's role delta, anthropic's `message_start`) satisfies it
        // before content arrives. This measures time-to-first-EVENT, the same quantity on both legs,
        // so the difference still isolates what the gateway added; milliseconds per sample rather
        // than the ~1.3s a full 64-frame paced stream takes.
        //
        // Percentile per leg, THEN differenced - the same shape `AddedLatency` publishes for the
        // non-streaming case - so the two "added" families mean the same thing.
        let ttft_samples =
            |addr: std::net::SocketAddr, path: &str, headers: &[(String, String)]| -> Vec<u64> {
                (0..STREAM_TTFT_SAMPLES)
                    .filter_map(|_| {
                        crate::http::post_json_sse(
                            addr,
                            path,
                            body.as_bytes(),
                            headers,
                            STREAM_TIMEOUT,
                            1,
                            Some(ctx.dialect),
                        )
                        .frame_offsets_us
                        .first()
                        .copied()
                    })
                    .collect()
            };
        let gw_path = crate::run::path_for(ctx.cfg, ctx.dialect, &ctx.id.egress);
        let direct_path = ctx.dialect.mock_direct_path(&model);
        let mut gw_ttfts = ttft_samples(ctx.cfg.gateway_addr, &gw_path, &gw_headers);
        let mut direct_ttfts = ttft_samples(ctx.cfg.mock_addr, &direct_path, &direct_headers);
        // Loss is reported, not merely counted: a leg shedding most of its probes still produces a
        // publishable percentile, and a large loss is worth a line on stderr while the run happens.
        for (leg, got) in [("gateway", gw_ttfts.len()), ("direct", direct_ttfts.len())] {
            if got < STREAM_TTFT_SAMPLES {
                eprintln!(
                    "streaming: the {leg} leg returned a first token on {got} of {STREAM_TTFT_SAMPLES} \
                     TTFT probes, so its added-TTFT percentiles carry that weight and no more"
                );
            }
        }
        let gw_n = gw_ttfts.len() as f64;
        let direct_n = direct_ttfts.len() as f64;
        gw_ttfts.sort_unstable();
        direct_ttfts.sort_unstable();
        // The rank comes from `stats::nearest_rank_index`, the engine's ONE percentile convention
        // (ledger SRCH-04: this used to reimplement its own ceil while other modules used floor,
        // disagreeing by a rank on every percentile whose `n * p` is a whole number).
        let ttft_pct = |v: &[u64], pct: f64| -> Option<f64> {
            if v.is_empty() {
                return None;
            }
            Some(v[crate::stats::nearest_rank_index(v.len(), pct)] as f64)
        };
        // Both percentiles come from the SAME samples, or they are not percentiles of one thing: an
        // earlier version took p99 from the sample set but left p50 as the single stream measured
        // above, which could (and did) publish a p99 below its own p50.
        let added_ttft_at = |pct: f64| {
            match (ttft_pct(&gw_ttfts, pct), ttft_pct(&direct_ttfts, pct)) {
            (Some(g), Some(d)) if g >= d => Measurement::Measured(g - d),
            // A gateway cannot be faster than the upstream it proxies, so a negative difference is
            // rig noise. `BelowResolution`, not `NotMeasured`: this is the comparison's best possible
            // outcome, and the site renders the two apart from a clamped-zero claim of no added cost.
            (Some(g), Some(d)) => Measurement::absent_because(
                Absent::BelowResolution,
                format!(
                    "the gateway's own time to first token at this percentile ({g:.0}us) came in under \
                     the mock's ({d:.0}us), which a proxy cannot really do - the added TTFT here is \
                     below what this rig can resolve"
                ),
            ),
            // Absent, not zero, when either leg produced nothing: a leg with no samples has no
            // percentile, and a 0 would read as "the gateway added nothing".
            _ => Measurement::absent_because(
                Absent::NotMeasured,
                format!(
                    "no time-to-first-token arrived on one of the two legs across {STREAM_TTFT_SAMPLES} samples, so there is no distribution to difference"
                ),
            ),
        }
        };
        let added_ttft_p50 = added_ttft_at(0.50);
        let added_ttft_p99 = added_ttft_at(0.99);

        // THE GAP DISTRIBUTION IS INSIDE ONE STREAM, and it is not small: a stream carries
        // `STREAM_FRAME_BUDGET` frames, so it yields that many gaps MINUS ONE. Nearest-rank through
        // `stats::nearest_rank_index`, the one convention `gen::GenStats::pct_of` and the search's
        // median now resolve through too, so a published percentile is always a gap some pair of
        // frames actually produced AND means the same thing as every other percentile on the board.
        let gap_pct = |o: &crate::http::SseOutcome, pct: f64| -> Option<f64> {
            gap_percentile_us(&o.frame_offsets_us, pct)
        };

        // Percentile per leg, THEN difference - same shape as `AddedLatency`'s
        // `gateway_c1_p99_us`/`direct_c1_p99_us`, so streaming and non-streaming "added" figures mean
        // the same thing. A negative raw difference is below-resolution rig noise, not a measured
        // zero: both legs carry the mock's ~20ms pacing, so this extracts a microsecond signal by
        // differencing two ~20,000us numbers, and a proxy cannot legitimately beat its own upstream.
        // Clamping to 0 would claim a precision this rig doesn't have.
        let added_gap_at = |pct: f64| {
            match (gap_pct(&through_gateway, pct), gap_pct(&direct, pct)) {
            (Some(g), Some(d)) if g >= d => Measurement::Measured(g - d),
            (Some(g), Some(d)) => Measurement::absent_because(
                Absent::BelowResolution,
                format!(
                    "the gateway's own inter-frame gap at this percentile ({g:.0}us) came in under the \
                     mock's ({d:.0}us), which a proxy cannot really do - the added gap here is below \
                     what this rig can resolve against the mock's {}ms pacing",
                    crate::run::stream_pacing_interval_ms()
                ),
            ),
            _ => Measurement::absent_because(
                Absent::NotMeasured,
                "a single frame on one of the two legs leaves no inter-frame gap to difference".to_string(),
            ),
        }
        };

        let fields: Filled = vec![
            ("added_ttft_p50_us", added_ttft_p50),
            ("added_ttft_p99_us", added_ttft_p99),
            ("added_gap_p50_us", added_gap_at(0.50)),
            ("added_gap_p99_us", added_gap_at(0.99)),
            (
                "gateway_c1_frames",
                Measurement::Measured(through_gateway.frame_offsets_us.len() as f64),
            ),
            (
                "direct_c1_frames",
                Measurement::Measured(direct.frame_offsets_us.len() as f64),
            ),
            // Always MEASURED, even at zero: "no probe came back" is itself the fact a reader needs
            // to judge the percentiles going missing.
            ("ttft_gw_samples", Measurement::Measured(gw_n)),
            ("ttft_direct_samples", Measurement::Measured(direct_n)),
        ];
        fields.into()
    }
}

/// Added latency: what the gateway adds to a single request's round trip at concurrency 1, over the
/// same request taken straight to the mock.
///
/// One group, not two: `added_latency_p99_us` and `gateway_c1_p99_us`/`direct_c1_p99_us` come from
/// the SAME two windows, and re-running either leg to fill a field the other forgot would put the
/// difference and its own operands on two different populations.
///
/// Concurrency 1 on purpose: added latency asks what ONE request costs with nothing else contending
/// for the gateway; any higher concurrency reintroduces queueing delay into a number meant to
/// isolate per-request overhead.
pub struct AddedLatency;

/// Whether a c=1 window's own reading may be trusted as a leg of the comparison: at least one
/// success and zero failures - the same clean-window bar `SweepProbe` uses for a throughput rung.
/// A free function so the boundary can be pinned directly without a subprocess load window behind it.
fn clean_c1_leg(ok: u64, fail: u64) -> bool {
    ok > 0 && fail == 0
}

/// The added-latency difference: gateway leg minus direct-to-mock leg, at microsecond resolution.
/// A gateway cannot legitimately answer faster than the upstream it proxies, so a negative raw
/// difference is rig noise (two separate processes/windows on a real box). `BelowResolution` rather
/// than a clamped 0 - the SAME rule as `Streaming::measure`'s `added_ttft`/`added_gap` - so all six
/// published differences say "too small to see" the same way instead of claiming a false precision.
fn added_latency_diff(gateway_us: u64, direct_us: u64) -> Measurement<f64> {
    if gateway_us >= direct_us {
        Measurement::Measured((gateway_us - direct_us) as f64)
    } else {
        Measurement::absent_because(
            Absent::BelowResolution,
            format!(
                "the gateway leg's c=1 reading ({gateway_us}us) came in under the direct-to-mock \
                 leg's ({direct_us}us), which a proxy cannot really do - the added latency here is \
                 below what this rig can resolve"
            ),
        )
    }
}

impl Metric for AddedLatency {
    fn name(&self) -> &'static str {
        "added_latency"
    }

    /// The last two are NOT published as their own artifact numbers - they feed `CellPerf.c1_note`,
    /// saying HOW MANY round trips each p99 was computed over (a p99 across four thousand samples and
    /// one across eleven are wildly different weight for the same field name). They ride the metric
    /// surface rather than being recomputed in `suite.rs` since the counts belong to the SAME windows
    /// the percentiles came from.
    fn fields(&self) -> &'static [&'static str] {
        &[
            "added_latency_p50_us",
            "added_latency_p99_us",
            "gateway_c1_p99_us",
            "direct_c1_p99_us",
            "gateway_c1_samples",
            "direct_c1_samples",
        ]
    }

    fn measure(&self, ctx: &CellCtx<'_>) -> Measured {
        let all_absent = |detail: String| -> Measured {
            let f: Filled = self
                .fields()
                .iter()
                .map(|x| {
                    (
                        *x,
                        Measurement::absent_because(Absent::NotMeasured, detail.clone()),
                    )
                })
                .collect();
            f.into()
        };

        // The cell's own model on BOTH legs (`run::model_for`): the gateway leg must reach this
        // cell's upstream, and the direct leg must ask the mock the same question.
        let model = crate::run::model_for(ctx.cfg, &ctx.id.egress);
        let body = ctx.dialect.body(&model);
        let gw_path = crate::run::path_for(ctx.cfg, ctx.dialect, &ctx.id.egress);
        let direct_path = ctx.dialect.mock_direct_path(&model);

        // The same duration every other window in this engine uses (`cfg.sweep_duration_s`), rather
        // than a second magic number - one knob for "how long is a load window" instead of two that
        // can silently drift apart. At c=1 the sample count is duration/RTT rather than
        // duration*concurrency, but still comfortably hundreds to low thousands of samples.
        //
        // The two legs authenticate differently on purpose: the gateway leg carries whatever routing
        // headers the manifest needs to select this egress column; the direct leg carries only the
        // dialect's own auth, since routing headers mean nothing to the mock.
        let gw_headers = crate::run::headers_for(ctx.cfg, ctx.dialect, &ctx.id.egress);
        let direct_headers = ctx.dialect.auth_headers(&ctx.cfg.auth);
        let gw = crate::run::load_window(ctx.cfg, &gw_path, &body, &gw_headers, 1);
        let direct = crate::run::load_window_at(
            ctx.cfg,
            ctx.cfg.mock_addr,
            &direct_path,
            &body,
            &direct_headers,
            1,
        );

        let (Some(gw), Some(direct)) = (gw, direct) else {
            return all_absent(
                "no concurrency-1 load window completed on one of the two legs, so there is nothing to difference"
                    .to_string(),
            );
        };
        // A leg with any failure is not a latency reading of that leg. The counts publish in the
        // detail since they ARE the finding when everything failed (the site renders "failed -
        // 0/14201 ok" from this sentence), and the budget-exceeded share separates a gateway refusal
        // from a response outrunning a bound of ours.
        let not_clean = |leg: &str, s: &crate::gen::GenStats| {
            let budget = if s.budget_exceeded > 0 {
                format!(
                    " ({} of them exceeded a bound of OURS rather than being refused: the response \
                     budget, or the connect budget)",
                    s.budget_exceeded
                )
            } else {
                String::new()
            };
            format!(
                "the {leg} leg at c=1 was not clean: {} ok, {} fail{budget}",
                s.ok, s.fail
            )
        };
        if !clean_c1_leg(gw.ok, gw.fail) {
            return all_absent(not_clean("gateway", &gw));
        }
        if !clean_c1_leg(direct.ok, direct.fail) {
            return all_absent(not_clean("direct-to-mock", &direct));
        }
        let (Some(gw_p99), Some(direct_p99)) = (gw.p99_us, direct.p99_us) else {
            return all_absent("one leg's c=1 window produced no p99 reading".to_string());
        };

        let added_p99 = added_latency_diff(gw_p99, direct_p99);
        let added_p50 = match (gw.p50_us, direct.p50_us) {
            (Some(g), Some(d)) => added_latency_diff(g, d),
            _ => Measurement::absent_because(
                Absent::NotMeasured,
                "one leg's c=1 window produced no p50 reading",
            ),
        };

        let fields: Filled = vec![
            ("added_latency_p50_us", added_p50),
            ("added_latency_p99_us", added_p99),
            ("gateway_c1_p99_us", Measurement::Measured(gw_p99 as f64)),
            ("direct_c1_p99_us", Measurement::Measured(direct_p99 as f64)),
            // Both legs are already known clean (`clean_c1_leg` returned true), so `ok` IS the count.
            ("gateway_c1_samples", Measurement::Measured(gw.ok as f64)),
            ("direct_c1_samples", Measurement::Measured(direct.ok as f64)),
        ];
        fields.into()
    }
}

// Sustained throughput was a metric group and is not one any more: it measured the highest
// concurrency holding p99 under a fixed ceiling with errors under 0.1%. The FRONTIER replaced it -
// rather than one scalar under one chosen ceiling, the board publishes a reading at each declared
// bound. The struct itself (`SustainedThroughput`, with no `impl Metric` and no `METRICS` entry) was
// deleted rather than left inert: unreachable documentation reads as a description of what runs.

// ── the two concurrent-stream groups ──────────────────────────────────────────────────────────────
//
// Two groups, not one, per this file's rule: numbers from ONE search share a group, numbers from
// SEPARATE searches do not. `streams_sustained`/`streams_sustained_fps` come from one
// `bisect_ceiling` over a monotone pass/fail gate, so they're one group like `Throughput`'s peak and
// its concurrency. A second group (`cpu_fps`, over a `saturation_plateau` search) was retired: 4 of
// 16 cells had it inverted below the proven delivery boundary, 5 were redundant, 7 were measured
// where the delivery gate did not hold, and the search function is now deleted.
//
// Sharing a window driver (`run::stream_window`) is NOT sharing a search: sharing the instrument is
// what makes numbers comparable, sharing a search is what makes them one population.

/// The inter-frame gap at a percentile, over the gaps INSIDE one stream.
///
/// A stream carrying `STREAM_FRAME_BUDGET` frames yields that many gaps minus one, so a gap
/// percentile is a real distribution even from a single stream - unlike time-to-first-token, of
/// which a stream produces exactly one.
///
/// Nearest-rank through `stats::nearest_rank_index`, the engine's single percentile convention, so a
/// published percentile is always a gap some pair of frames actually produced, never an
/// interpolation. `None` when there is no gap at all: a single frame has no inter-frame time, and a
/// zero there would read as instant delivery.
fn gap_percentile_us(frame_offsets_us: &[u64], pct: f64) -> Option<f64> {
    let mut gaps: Vec<u64> = frame_offsets_us
        .windows(2)
        .map(|w| w[1].saturating_sub(w[0]))
        .collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_unstable();
    Some(gaps[crate::stats::nearest_rank_index(gaps.len(), pct)] as f64)
}

// The stream searches take the engine's full ceiling, like the throughput searches always did:
// `run::stream_window` drives one tokio task per lane instead of one OS thread, so the old clamp to
// avoid 65536-thread scheduler thrashing no longer applies.

/// Which side of the cell cannot be streamed, if either.
///
/// The frames come from the MOCK, standing in for the upstream, so a cell can only be streamed when
/// BOTH ends can carry one: the ingress dialect must be posable as a stream, and the egress upstream
/// must answer with real SSE frames. Only openai and anthropic do (`Dialect::streams_natively`).
///
/// Guarding on the ingress alone checks the wrong end: an ingress that streams paired with an egress
/// that doesn't (e.g. bedrock/cohere/gemini) makes the mock produce zero frames at every concurrency,
/// which reads as the gateway failing to stream when it is actually a rig limit.
fn stream_blocked_by(ctx: &CellCtx<'_>) -> Option<String> {
    if !ctx.dialect.streams_natively() {
        return Some(ctx.dialect.as_str().to_string());
    }
    // An egress the mock cannot stream blocks the cell just as completely - it's the end the frames
    // actually come from. An egress that does not parse as a dialect is left alone.
    match ctx.id.egress.parse::<crate::ingress::Dialect>() {
        Ok(eg) if !eg.streams_natively() => Some(eg.as_str().to_string()),
        _ => None,
    }
}

/// A dialect the mock cannot stream is a rig limit, not a gateway failure - must be stated
/// identically to `Streaming::measure`'s opening fact, or the same rig limit is published two ways.
fn stream_untestable_named(side: &str) -> Measurement<f64> {
    Measurement::absent_because(
        Absent::Untestable,
        format!(
            "the mock does not answer {side} with a native event stream, so the rig cannot pose the streaming question here"
        ),
    )
}

/// Streams sustained: the highest number of concurrent streams the gateway carries while nearly every
/// expected frame still arrives, nothing stalls past twice the mock's pace, and almost no stream
/// fails - plus the frames/sec it carries THERE.
pub struct StreamsSustained;

impl Metric for StreamsSustained {
    fn name(&self) -> &'static str {
        "streams_sustained"
    }

    fn fields(&self) -> &'static [&'static str] {
        &["streams_sustained", "streams_sustained_fps"]
    }

    fn measure(&self, ctx: &CellCtx<'_>) -> Measured {
        if let Some(side) = stream_blocked_by(ctx) {
            let m = stream_untestable_named(&side);
            let f: Filled = self.fields().iter().map(|x| (*x, m.clone())).collect();
            return f.into();
        }
        let found = crate::run::sweep_streams_cell(ctx.cfg, ctx.id, ctx.min_conc, ctx.max_conc);
        let carry = |m: &Measurement<f64>| match (m.reason().cloned(), m.detail()) {
            (Some(r), Some(d)) => Measurement::absent_because(r, d),
            (Some(r), None) => Measurement::absent(r),
            (None, _) => Measurement::absent(Absent::NotMeasured),
        };
        let fps = match found.fps.value() {
            Some(v) => Measurement::Measured(*v),
            None => carry(&found.fps),
        };
        // The headline field keeps its evidence: `fps` goes through `carry`, which preserves the
        // detail, so `streams_sustained` (the field this group is named after) doesn't publish a
        // bare token while its sibling `fps` gets an explanation. Also prefers the CONCURRENCY's own
        // reason where it has one, rather than always mirroring the rate's.
        let conc = match found.concurrency.value() {
            Some(c) => Measurement::Measured(f64::from(*c)),
            None => match (
                found.concurrency.reason().cloned(),
                found.concurrency.detail(),
            ) {
                (Some(r), Some(d)) => Measurement::absent_because(r, d),
                (Some(r), None) => Measurement::absent(r),
                // No reason of its own: the rate came from the same search, so its reason is the
                // honest stand-in - detail included, which is the whole point.
                (None, _) => carry(&found.fps),
            },
        };
        Measured {
            fields: vec![("streams_sustained", conc), ("streams_sustained_fps", fps)],
            series: Series {
                sweep_streams: found
                    .points
                    .iter()
                    .map(crate::run::StreamPoint::to_json)
                    .collect(),
                ..Series::default()
            },
        }
    }
}

/// What the cell cost, at one concurrency shared by every gateway.
///
/// The throughput ladder stops answering "how fast" the moment a gateway saturates its pinned cores
/// - past that it measures the box. Cost per request has no such ceiling: at saturation two gateways
/// deliver the same rps by definition, and the one doing less work per request still reads lower.
///
/// One concurrency, declared, same for everyone: there is no concurrency that is sub-saturation for
/// every entrant (this field spans 19 rps to 49,000), so matched CONCURRENCY is the honest
/// substitute for matched load, published beside the cost so a reader knows what was held constant.
pub struct Cost;

/// The rung every gateway's cost is taken at. Small enough that fast entrants aren't yet queueing on
/// their own cores, large enough that a gateway which only performs with concurrency isn't judged on
/// a serial round trip. NOT tuned per gateway - varying it by entrant would break the comparison.
pub const COST_WINDOW_CONCURRENCY: u32 = 8;

/// Why a cell has no time-to-plateau, in the words a reader sees.
///
/// Names the SHAPE, because "no steady state" alone describes two opposite gateways: one still
/// releasing memory reads the same as one still climbing toward a leak without it. `Shape` exists
/// for exactly this distinction.
///
/// Pure, so the wording can be tested: this string is the whole finding for a reader who never opens
/// the artifact.
fn no_plateau_detail(verdict: &crate::stats::Verdict) -> String {
    match verdict {
        crate::stats::Verdict::NotSteady { shape, .. } => format!(
            "memory was still {} when the {MEMORY_LOAD_S}s load window closed, so it has no \
             time-to-plateau; its rate is published beside this",
            match shape {
                crate::stats::Shape::Climbing => "climbing",
                crate::stats::Shape::Falling => "falling - releasing memory, not leaking it",
                crate::stats::Shape::Oscillating =>
                    "oscillating with no net trend - it returns to where it was, which is not a leak",
            }
        ),
        _ => "memory reached no steady state inside the load cap, so there is no time-to-plateau"
            .to_string(),
    }
}

impl Metric for Cost {
    fn name(&self) -> &'static str {
        "cost"
    }

    fn fields(&self) -> &'static [&'static str] {
        &[
            "cpu_us_per_request",
            "rps_per_cpu_second",
            "cost_window_conc",
            "cost_window_ok",
            "cost_window_rps",
            "cost_core_utilisation",
            "cost_threads",
            "cost_nonvol_ctxt_per_request",
            "cost_majflt",
        ]
    }

    fn measure(&self, ctx: &CellCtx<'_>) -> Measured {
        let all_absent = |detail: String| -> Measured {
            let f: Filled = self
                .fields()
                .iter()
                .map(|x| {
                    (
                        *x,
                        Measurement::absent_because(Absent::NotMeasured, detail.clone()),
                    )
                })
                .collect();
            f.into()
        };

        // The same request every other window on this cell drives, from the same helpers.
        let model = crate::run::model_for(ctx.cfg, &ctx.id.egress);
        let body = ctx.dialect.body(&model);
        let path = crate::run::path_for(ctx.cfg, ctx.dialect, &ctx.id.egress);
        let headers = crate::run::headers_for(ctx.cfg, ctx.dialect, &ctx.id.egress);

        let (stats, cost, util) = crate::run::load_window_costed(
            ctx.cfg,
            &path,
            &body,
            &headers,
            COST_WINDOW_CONCURRENCY,
        );

        let Some(stats) = stats else {
            return all_absent(format!(
                "no load window completed at c={COST_WINDOW_CONCURRENCY}, so there is nothing to \
                 charge a cost against"
            ));
        };
        // A window with failures is not a cost reading: CPU spent refusing requests is real CPU, but
        // dividing it by only the successes would report a mostly-failing gateway as extravagantly
        // expensive - a statement about the failure, not the work.
        if stats.fail > 0 {
            return all_absent(format!(
                "the c={COST_WINDOW_CONCURRENCY} cost window had {} failure(s) alongside {} success(es); \
                 CPU divided by only the successes would describe the failures, not the work",
                stats.fail, stats.ok
            ));
        }

        let mut f: Filled = vec![
            ("cpu_us_per_request", cost.cpu_us_per_request),
            ("rps_per_cpu_second", cost.rps_per_cpu_second),
            (
                "cost_window_conc",
                Measurement::Measured(f64::from(COST_WINDOW_CONCURRENCY)),
            ),
            // The window's own load, published so the cost is CHECKABLE: without these,
            // `cpu_us_per_request` cannot be re-derived from what's published beside it, and a low
            // utilisation could mean either a cheap gateway or an under-loaded window - two opposite
            // readings the artifact must be able to tell apart.
            ("cost_window_ok", Measurement::Measured(stats.ok as f64)),
            ("cost_window_rps", Measurement::Measured(stats.rps())),
            // Whether the peak is a ceiling: at ~1.0 the gateway filled its cores and the throughput
            // number is a wall; well below it, the limit is elsewhere.
            ("cost_core_utilisation", util),
            ("cost_threads", cost.threads_end),
            ("cost_nonvol_ctxt_per_request", cost.nonvol_ctxt_per_request),
            ("cost_majflt", cost.majflt),
        ];
        // A swapping box is not a slow gateway: major faults mean pages came from disk, so what was
        // timed is the disk. Numbers still publish (a reader must see why the row looks wrong) but
        // the cost figures are re-flagged HarnessError so nothing ranks on them.
        if cost.swapped {
            let why = "the box took major page faults during this window, so it was swapping and \
                       this cost describes the disk rather than the gateway"
                .to_string();
            for (name, m) in f.iter_mut() {
                if matches!(*name, "cpu_us_per_request" | "rps_per_cpu_second") {
                    *m = Measurement::absent_because(Absent::HarnessError, why.clone());
                }
            }
        }
        f.into()
    }
}

#[cfg(test)]
mod tests {
    // A gateway handing memory back must never read like one leaking it: `Shape` already separates
    // climbing/falling/oscillating, and the reader-facing string must reflect that distinction.
    #[test]
    fn the_no_plateau_reason_says_which_way_memory_was_moving() {
        use crate::stats::{Shape, Verdict};
        let rate = crate::measurement::Measurement::Measured(-0.43);
        let falling = no_plateau_detail(&Verdict::NotSteady {
            growth_rate_mib_per_min: rate.clone(),
            shape: Shape::Falling,
        });
        assert!(falling.contains("falling"), "{falling}");
        assert!(falling.contains("not leaking it"), "a falling gateway must be told apart from a leak: {falling}");

        let climbing = no_plateau_detail(&Verdict::NotSteady {
            growth_rate_mib_per_min: rate.clone(),
            shape: Shape::Climbing,
        });
        assert!(climbing.contains("climbing"), "{climbing}");
        assert!(!climbing.contains("not leaking"), "a climbing gateway must NOT be excused: {climbing}");

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

        // A window that could not be JUDGED is a different claim from one that moved, and keeps the
        // wording that says so rather than borrowing a shape it never established.
        let undecidable = no_plateau_detail(&Verdict::Undecidable(
            crate::stats::Undecidable::WindowTooShort,
        ));
        assert!(undecidable.contains("no steady state"), "{undecidable}");
        assert!(!undecidable.contains("climbing") && !undecidable.contains("falling"), "{undecidable}");
    }

    // Every `Series` field must survive the accumulator in `process_cell_with` (see its comment on
    // `idle_rss` above for the regression this guards). Drives a metric with EVERY field populated
    // and asserts every one arrives, so a field added to `Series` and forgotten in the merge fails
    // here instead of publishing silence.
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
                        frontier: Vec::new(),
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

    // A six-second window cannot settle a sixty-second question. See `window_is_long_enough`'s doc.
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

    // A dead sampler must not read as a settled gateway. See `steady_is_believable`'s doc.
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
    /// an absence carrying a reason, never leave the key out - a missing key and a null are different
    /// statements.
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

        let filled: BTreeMap<&'static str, Measurement<f64>> =
            Forgetful.measure(&ctx).fields.into_iter().collect();
        let mut out = BTreeMap::new();
        for field in Forgetful.fields() {
            let value = filled.get(field).cloned().unwrap_or_else(|| {
                Measurement::absent_because(
                    Absent::NotMeasured,
                    format!(
                        "the {} group declares {field} but returned no value for it",
                        Forgetful.name()
                    ),
                )
            });
            out.insert(*field, value);
        }

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
    #[test]
    fn idle_memory_is_absent_when_the_gateway_cannot_be_returned_to_rest() {
        let cfg = a_config();
        assert!(
            cfg.relaunch.is_none(),
            "this fixture owns no gateway lifetime"
        );
        let id = CellId::new("openai", "openai");
        let ctx = CellCtx {
            cfg: &cfg,
            id: &id,
            dialect: Dialect::Openai,
            min_conc: 1,
            max_conc: 2,
        };
        let filled: BTreeMap<_, _> = Memory.measure(&ctx).fields.into_iter().collect();
        let idle = filled
            .get("memory_idle_mib")
            .expect("the memory group declares memory_idle_mib");
        assert_eq!(
            idle.copied(),
            None,
            "idle must not be published from a process that served load"
        );
        assert!(
            idle.reason().is_some(),
            "an absent idle must carry the reason it could not be taken, not a bare null"
        );
    }

    // A failed restart aborts the WHOLE memory group (every field absent, reason HarnessError, no
    // series produced) rather than falling through to measure a gateway in an unknown state.
    //
    // Fixture: a real marker process stands in for the gateway tree so `root_pid` resolves; the
    // relaunch spec's stop path matches nothing (so stopping "succeeds" instantly) while its binary
    // doesn't exist, so `restart_to_rest` fails fast on any platform.
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
        let mut child = std::process::Command::new("python3")
            .args(["-c", "import time,sys; time.sleep(120)", &marker])
            .spawn()
            .expect("spawn a marker process to stand in for the gateway tree");

        let mut cfg = a_config();
        cfg.runtime = crate::manifest::Runtime::Native {
            proc_match: marker.clone(),
        };
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0")
                .expect("bind an ephemeral port to pick one");
            l.local_addr().expect("addr").port()
        };
        cfg.relaunch = Some(crate::launch::LaunchSpec {
            runtime: crate::manifest::Runtime::Native {
                // Deliberately NOT the marker: the stop path must succeed (nothing to stop) so the
                // failure under test is the relaunch itself.
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
        let _ = child.kill();
        let _ = child.wait();

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

    // Being IN `METRICS` is what makes a group reachable; this test names the field the two tests
    // above would silently miss if `AddedLatency` were dropped from the list without anything else
    // failing. Also covers the concurrent-stream group (`cpu_fps` is retired - see the note above
    // `StreamsSustained`).
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

    // The frames come from the egress, so the egress decides whether there are any - see
    // `stream_blocked_by`'s doc for why guarding on ingress alone is wrong.
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

    // The gap distribution is inside the stream: unlike TTFT (one per stream), a stream carrying
    // STREAM_FRAME_BUDGET frames yields that many gaps minus one, so it is a real distribution.
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

    // A column that can never hold a number is either measured or deleted: one stream yields exactly
    // one time-to-first-token, so a real p99 needs samples, which are cheap (a TTFT reads ONE frame
    // and stops).
    #[test]
    fn a_ttft_percentile_needs_samples_and_the_sample_count_makes_one_real() {
        // 100 is the smallest count where a 99th percentile is a real order statistic rather than a
        // restatement of the maximum: nearest-rank puts it at index 99 of 100, not at the top.
        // (No `assert!(SAMPLES >= 100)`: an assertion over a constant cannot fail. The rank checks
        // below use the constant itself and would break if it were lowered, which is the real check.)
        // Uses the ENGINE's own `stats::nearest_rank_index` (what `ttft_pct` calls) rather than a
        // formula retyped here, per ledger SRCH-04 (a retyped ceil vs. floor mismatch).
        let idx_of = crate::stats::nearest_rank_index;
        assert_eq!(idx_of(STREAM_TTFT_SAMPLES, 0.99), 98);
        assert!(
            idx_of(STREAM_TTFT_SAMPLES, 0.99) < STREAM_TTFT_SAMPLES - 1,
            "the p99 must not be the max, or it is not a percentile"
        );
        // One sample cannot support a percentile: the p99 IS that sample.
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

    // What a cell cost, per group, in the artifact: a wall-clock total cannot answer what made a slow
    // run slow, but per-group seconds make it arithmetic on committed JSON instead of a stopwatch rerun.
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
        let (_, _, all) = process_cell_with(&ctx, METRICS);
        assert_eq!(all.len(), METRICS.len(), "every group must report: {all:?}");
        for m in METRICS {
            assert!(all.contains_key(m.name()), "{} reported no cost", m.name());
        }
    }

    // A p99 below its own p50 is two populations wearing one name - no percentile pair over one
    // distribution can produce it. Asserted here as an ordering over one sample set.
    #[test]
    fn the_ttft_percentiles_come_from_one_sample_set_so_p99_can_never_sit_below_p50() {
        let pct = |v: &[u64], p: f64| {
            let rank = (((v.len() as f64) * p).ceil() as usize).clamp(1, v.len());
            v[rank - 1] as f64
        };
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

        // Differencing keeps the ordering: both legs are percentiles of the same rank.
        let direct: Vec<u64> = samples.iter().map(|v| v / 2).collect();
        let add = |p: f64| (pct(&samples, p) - pct(&direct, p)).max(0.0);
        assert!(
            add(0.99) >= add(0.50),
            "the ADDED figures must hold the same ordering"
        );

        // One sample cannot support a p99 that means anything: it equals the p50 rather than sitting
        // below it.
        assert_eq!(pct(&[500], 0.99), pct(&[500], 0.50));
    }

    // A difference the rig cannot see is not a measured zero: clamping a negative raw diff to 0
    // claims a precision this rig doesn't have and can produce a p99 below its own p50.
    #[test]
    fn a_percentile_difference_below_the_rigs_resolution_is_absent_not_zero() {
        // The rule, stated over the raw pair the engine differences.
        let judge = |gw: f64, mock: f64| -> Option<f64> {
            if gw >= mock {
                Some(gw - mock)
            } else {
                None
            }
        };

        // A real addition survives, unchanged.
        assert_eq!(judge(20_015.0, 20_000.0), Some(15.0));
        // Equal legs are a genuine, measurable zero: the gateway added nothing detectable AND the
        // comparison was valid. That must still publish 0, not absent.
        assert_eq!(judge(20_000.0, 20_000.0), Some(0.0));
        // The gateway "faster" than the upstream it proxies is the impossible case.
        assert_eq!(
            judge(19_996.0, 20_000.0),
            None,
            "a proxy cannot beat its own upstream"
        );

        // And the property that was violated: whatever the rule returns, a p99 that IS published can
        // never sit below a p50 that is published from the same distribution.
        let samples: Vec<f64> = (0..100).map(|i| 20_000.0 + f64::from(i % 7)).collect();
        let pct = |v: &[f64], p: f64| {
            let mut s = v.to_vec();
            s.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let rank = (((s.len() as f64) * p).ceil() as usize).clamp(1, s.len());
            s[rank - 1]
        };
        let mock: Vec<f64> = samples.iter().map(|v| v - 3.0).collect();
        let p50 = judge(pct(&samples, 0.50), pct(&mock, 0.50));
        let p99 = judge(pct(&samples, 0.99), pct(&mock, 0.99));
        if let (Some(a), Some(b)) = (p50, p99) {
            assert!(b >= a, "p99 {b} sits below p50 {a}");
        }
    }
}
