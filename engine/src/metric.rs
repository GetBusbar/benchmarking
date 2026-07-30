// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// THE ENGINE, STATED ONCE: for every configured cell, run every metric.
//
// WHY THIS EXISTS. A metric is in `METRICS` or it does not exist, and `METRICS` is one thing a
// human can read in full: a module can be finished and unit-tested against fakes with nothing in
// the real run ever calling it, and a per-module test suite reporting green cannot catch that a
// module is unreachable. `site/gen-data.mjs` takes memory SOLELY from the per-cell window, with no
// fallback, so an unreachable `rss` module would mean a board that publishes no memory at all. This
// list is the one place a measurement being "implemented, tested, and silently never taken" cannot
// happen.
//
// WHY A GROUP AND NOT A FUNCTION PER NUMBER. The obvious shape is one metric per published field.
// It is wrong on the physics: idle, peak, high-water and recovered RSS are four readings of ONE load
// window, and a peak search yields the peak AND the concurrency it happened at from ONE search.
// Splitting those into separate metrics would re-run the window and re-run the search, which is both
// slower and, worse, DIFFERENT - two windows are two populations, and publishing an idle from one
// beside a peak from another is exactly the two-populations defect this rewrite exists to end.
//
// So the unit is a procedure with several named outputs. `fields()` declares what a group promises
// to fill; `measure()` returns what it actually filled. The two are checked against each other, so a
// group that quietly returns fewer numbers than it advertises is a test failure rather than a hole
// in the artifact.
//
// EVERY OUTPUT IS A `Measurement`. Not an f64, not an Option. A metric that cannot measure returns
// an absence WITH A REASON, and there is no way to return a bare number instead. That invariant kept
// being violated one wiring at a time precisely because each call site re-decided how to represent
// "we didn't get it".

use crate::cell::CellId;
use crate::ingress::Dialect;
use crate::measurement::{Absent, Measurement};
use crate::run::RunConfig;
use std::collections::BTreeMap;

/// Everything a metric is allowed to know about the cell it is measuring.
///
/// Deliberately small. A metric gets the cell's identity and the rig's configuration and nothing
/// else - in particular it does not get the gateway's capability declaration, because `probe.rs`
/// already records what happened when a declaration was allowed to reach a measurement decision: the
/// same observation was published two different ways, and the declared cell was tried harder.
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

/// THE EVIDENCE BEHIND THE SCALARS.
///
/// A group's headline numbers are summaries: a peak is one point out of a sweep, an idle and a peak
/// RSS are two readings out of a series. Until this existed there was nowhere for the underlying
/// points to go - `measure()` returned scalars and nothing else - so the searches collected their
/// probed points and the memory sampler collected its readings, and both were dropped on the floor
/// at the trait boundary. The published artifact carried `sweep_max_proxy: []` and `rss_series: []`
/// on every cell, which means no number on the board could be re-derived, charted, or checked
/// against the measurement it came from.
///
/// Empty is honest and common: a group that took no series simply returns none.
#[derive(Default)]
pub struct Series {
    /// One entry per concurrency the throughput search actually probed, in probe order.
    pub sweep: Vec<crate::record::SweepPoint>,
    /// One entry per concurrency the SUSTAINED-throughput search actually probed, in probe order.
    /// Kept apart from `sweep` above rather than merged into it: the two are two different searches
    /// (a unimodal max search and a monotone gate bisection) over the same concurrency axis, and
    /// merging their rungs would make it impossible to tell which point came from which search.
    pub sweep_sustained: Vec<crate::record::SweepPoint>,
    /// One entry per resident-memory reading taken across the load window.
    pub rss: Vec<crate::record::RssSample>,
    /// One entry per reading taken across the IDLE window, before any load. Kept apart from `rss`
    /// rather than prepended to it: they answer different questions (what it costs doing nothing,
    /// versus what work costs it), and a reader must be able to see the idle window's own shape to
    /// judge whether the baseline every other memory figure is measured against was itself steady.
    pub idle_rss: Vec<crate::record::RssSample>,
    /// One entry per concurrency the STREAMS-SUSTAINED gate search probed, and one per concurrency
    /// the CPU-frames/sec peak search probed. Kept apart from each other and from the two request
    /// sweeps above for the same reason those two are kept apart: four searches over one concurrency
    /// axis, and merging any pair of them would make it impossible to say which search a rung came
    /// from or which gate it was judged against.
    ///
    /// `serde_json::Value` rather than `SweepPoint`, because `record.rs` types these two as opaque
    /// JSON: no committed snapshot has ever carried one, so there was no real artifact to pin a shape
    /// against. `run::StreamPoint::to_json` is where the shape is decided.
    pub sweep_streams: Vec<serde_json::Value>,
    pub sweep_cpu_fps: Vec<serde_json::Value>,
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

/// THE ENGINE'S ENTIRE MEASUREMENT SURFACE.
///
/// Adding a number to the board is: implement a group, add it here. Removing one is deleting it from
/// this list, which is a visible act rather than a call that quietly stopped happening.
pub const METRICS: &[&dyn Metric] = &[
    &Throughput,
    &Memory,
    &Streaming,
    &AddedLatency,
    &StreamsSustained,
    &CpuFps,
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

/// The same loop over an EXPLICIT list.
///
/// `METRICS` is the engine's real surface, but a caller that reads it from a global cannot be tested
/// without performing every measurement for real - which is how adding the streaming group turned a
/// 0.4 second unit suite into a 160 second one, two twenty-second network timeouts per cell against a
/// fixture that holds its connection open. A test that slow stops being run, and a gate that stops
/// being run is not a gate.
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
        // ONE LINE PER GROUP, BEFORE IT RUNS, to stderr. A cell's wall clock is dominated by these
        // groups, not by the probe, so a run that only speaks when a cell FINISHES goes dark for
        // minutes at a time and an operator cannot tell a slow sweep from a wedged box. Printed
        // before rather than after: the interesting case is the group that never returns.
        // TIMED, AND THE TIME IS PUBLISHED. A run that is slower than the last one is a question
        // nobody can answer from a wall-clock total: "agentgateway took 13 minutes a cell" does not
        // say whether that was the TTFT distribution, a stream ladder climbing to a higher rung, or a
        // gateway that got slower. Each group's own seconds are recorded per cell, so the answer is
        // arithmetic on the artifact rather than a rerun with a stopwatch.
        //
        // Printed before AND after: before, because the interesting case is the group that never
        // returns and an operator watching a live box needs to see which one it was; after, because
        // that line is what makes the cost greppable out of a finished run's log.
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
        // Series ACCUMULATE across groups rather than overwrite: the sweep comes from throughput and
        // the readings come from memory, and a later group returning none must not erase an earlier
        // group's evidence.
        if !produced.series.sweep.is_empty() {
            series.sweep = produced.series.sweep;
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
        if !produced.series.sweep_cpu_fps.is_empty() {
            series.sweep_cpu_fps = produced.series.sweep_cpu_fps;
        }
        // THE IDLE WINDOW WAS MEASURED AND THEN DROPPED ON THE FLOOR. The memory group samples a full
        // idle window and returns it as `Series.idle_rss`, and this accumulator - a hand-written chain
        // with one clause per field - simply had no clause for it. So `CellMemory.idle_rss_series` was
        // published empty on every cell, the site's idle sparkline had nothing to draw, and the idle
        // verdict beside it described a series no reader could see. Nothing failed: an accumulator that
        // forgets a field looks exactly like a group that produced none.
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

    fn fields(&self) -> &'static [&'static str] {
        &[
            "rps_max_proxy",
            "conc_at_peak",
            "rps_sustained_20ms",
            "rps_sustained_20ms_concurrency",
            "conc_at_sustained",
        ]
    }

    fn measure(&self, ctx: &CellCtx<'_>) -> Measured {
        let perf = crate::run::sweep_cell(ctx.cfg, ctx.id, ctx.min_conc, ctx.max_conc);
        // The search's reason AND its evidence travel with the absence. A peak search that ran out
        // of range publishes a lower bound as prose; flattening that to a bare null is the one place
        // "the engine discards the measurement" was literally true.
        let carry = |m: &Measurement<f64>| match (m.reason().cloned(), m.detail()) {
            (Some(r), Some(d)) => Measurement::absent_because(r, d),
            (Some(r), None) => Measurement::absent(r),
            (None, _) => Measurement::absent(Absent::NotMeasured),
        };
        let rps = match perf.max_proxy.value() {
            Some(v) => Measurement::Measured(*v),
            None => carry(&perf.max_proxy),
        };
        // Mirrors the rps reason rather than inventing a second one: two different explanations for
        // one absence, in one cell, is a smaller version of the reason-swapping `Measurement` exists
        // to prevent.
        let conc = match perf.max_proxy_concurrency.value() {
            Some(c) => Measurement::Measured(f64::from(*c)),
            None => Measurement::absent(
                perf.max_proxy
                    .reason()
                    .cloned()
                    .unwrap_or(Absent::NotMeasured),
            ),
        };
        // THE SWEEP TRAVELS WITH THE PEAK. Each probed rung becomes a published point, so a reader
        // can see the shape the search walked and re-derive the maximum rather than trusting it.
        //
        // `p99_us` and `fail` come from the window itself. They used to be published absent here,
        // under the true-at-the-time note that the search's gate recorded only whether a rung PASSED
        // and not the latency behind that verdict - but the generator had measured both all along
        // and `Sample` was throwing them away. Re-deriving the maximum was possible from these
        // points; re-deriving the 20ms answer was not, which is why the engine went and measured the
        // cell a second time to get it. A rung that somehow arrives without a reading is still
        // absent rather than zero, because "measured no failures" and "nothing was measured" are
        // different facts and only one of them is true.
        let sweep = perf
            .points
            .iter()
            .map(|pt| crate::record::SweepPoint {
                conc: i64::from(pt.concurrency),
                rps: Measurement::Measured(pt.value as i64),
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
        // THE SECOND QUESTION, OFF THE SAME RUNGS. `sweep_cell` read this out of the windows the
        // climb had already taken, so it needs no measurement of its own and cannot describe a
        // different state of the gateway than `rps_max_proxy` does.
        let s_rps = match perf.sustained.value() {
            Some(v) => Measurement::Measured(*v),
            None => carry(&perf.sustained),
        };
        let s_conc = match perf.sustained_concurrency.value() {
            Some(c) => Measurement::Measured(f64::from(*c)),
            None => Measurement::absent(
                perf.sustained
                    .reason()
                    .cloned()
                    .unwrap_or(Absent::NotMeasured),
            ),
        };
        // The rungs as the gate saw them, plus the windows spent refining the boundary.
        let sweep_sustained = sustained_evidence(&perf.sustained_points);
        Measured {
            fields: vec![
                ("rps_max_proxy", rps),
                ("conc_at_peak", conc),
                ("rps_sustained_20ms", s_rps),
                ("rps_sustained_20ms_concurrency", s_conc.clone()),
                ("conc_at_sustained", s_conc),
            ],
            series: Series {
                sweep,
                sweep_sustained,
                ..Series::default()
            },
        }
    }
}

/// The sustained search's rungs as PUBLISHED EVIDENCE rows.
///
/// A free function so the one mapping that decides what a rung's evidence says can be pinned against
/// fixed points, rather than only through `Throughput::measure`, which needs a live gateway and a
/// live mock behind it to reach at all.
fn sustained_evidence(points: &[crate::run::SustainedPoint]) -> Vec<crate::record::SweepPoint> {
    points
        .iter()
        .map(|pt| crate::record::SweepPoint {
            conc: i64::from(pt.concurrency),
            rps: Measurement::Measured(pt.rps as i64),
            p99_us: match pt.p99_us {
                Some(v) => Measurement::Measured(v as i64),
                None => Measurement::absent(Absent::NotMeasured),
            },
            // ABSENT WHEN NO WINDOW CARRIED A READING, never a zero. This was
            // `Measured(pt.fail)` over an `i64` that had no way to be absent, so a rung whose
            // windows produced nothing published `fail: 0` - a number saying the gateway lost
            // nothing at a rate it was never observed serving. The reason travels with it: an
            // evidence row a reader cannot re-derive the verdict from is the defect class this whole
            // series exists to avoid.
            fail: match pt.fail {
                Some(f) => Measurement::Measured(f),
                None => Measurement::absent_because(
                    Absent::NotMeasured,
                    format!(
                        "no window at c={} came back with a reading, so this rung has no failure count",
                        pt.concurrency
                    ),
                ),
            },
        })
        .collect()
}

/// The concurrency the memory window runs at.
///
/// A CONSTANT, not the cell's peak, and that is the whole point. Memory is compared ACROSS gateways,
/// so every gateway's window must be the same load; taking each one at its own peak concurrency
/// would measure thirteen different workloads and rank them as if they were one. It is deliberately
/// not derived from core count either: the search maxima are, because a search explores the box, but
/// a comparison recipe that moves with the hardware makes two boxes' numbers incomparable.
pub const MEMORY_WINDOW_CONCURRENCY: u32 = 32;

/// How often the resident-memory sampler reads the tree during the window.
const MEMORY_SAMPLE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// LOAD RUNS UNTIL MEMORY STOPS MOVING, NOT FOR A FIXED TIME.
///
/// A fixed window reports where memory happened to be when we stopped watching. If a gateway is
/// still climbing when the window ends, its "peak" is a property of our stopwatch rather than of the
/// gateway, and two gateways that settle at different speeds are compared at different points on
/// their own curves. Running until the trailing window is flat measures the same THING on every
/// entrant: where it actually levels off.
///
/// The three-way verdict is why this is worth doing at all. `Steady` is a settled number.
/// `NotSteady` carries the growth rate, so a gateway that never levels off is published as exactly
/// that, with how fast it climbed, which is a more useful finding than any peak. `Undecidable` means
/// too few samples to judge, which is deliberately NOT the same claim as "it moved".
///
/// The cap is not a fallback, it is the whole reason a leak terminates: a gateway that never settles
/// would otherwise run forever. Hitting it is a result (`NotSteady`), never an error.
pub const MEMORY_PLATEAU_WINDOW_S: f64 = 60.0;
pub const MEMORY_MAX_LOAD_S: u64 = 300;

/// AND THEN WATCH IT FOR A MINUTE WITH THE LOAD GONE.
///
/// Peak answers what a gateway costs while working. It does not answer whether it gives any of that
/// back, and those are different questions about a service that will run for months. A gateway that
/// climbs to 120 MiB and returns to 8 is a different proposition from one that climbs to 120 and
/// stays there, and a peak alone cannot tell them apart.
///
/// So load stops, sampling continues for this long, and the trailing reading is published as
/// recovered. The same 60 seconds as the settle window, so the two halves of the curve are directly
/// comparable to a reader.
pub const MEMORY_RECOVERY_S: u64 = 60;
/// The trailing slice of that window the published `recovered_rss_mib` is the MEDIAN OF.
///
/// Not the whole 60 s, deliberately: the first half still holds the descent from peak as allocators
/// return pages, so a median over the full window would sit between the loaded and the recovered level
/// and report neither. The trailing half is the part that has stopped moving.
///
/// NAMED, AND PUBLISHED, because it was neither. It was the expression `MEMORY_RECOVERY_S / 2.0` inline
/// at the one call site, while `CellMemory.recovery_window_s` published 60 and the chart subtitle read
/// "recovered RSS at the end of the 60 s recovery window". The number was a median over the trailing 30.
/// A gateway still releasing memory across that minute therefore published a figure the stated window
/// would not produce, and nothing in the artifact let a reader tell. The artifact now discloses the slice
/// the number actually came from.
pub const MEMORY_RECOVERY_MEDIAN_S: u64 = MEMORY_RECOVERY_S / 2;
/// How long the process is watched BEFORE any load, and why it is a window rather than a reading.
///
/// Idle used to be one instantaneous sample taken the moment the restart returned. Two things are
/// wrong with that. A process that is still settling - lazy allocation, warm-up threads, a runtime
/// still building its pools - reads momentarily LOW, and every growth figure derived from idle is
/// then overstated, on a column the board ranks ascending. And a gateway that leaks while doing
/// NOTHING is invisible: with a single sample there is no second point to compare against.
///
/// The same 60 seconds as the recovery window, deliberately. It makes idle and `recovered_rss_mib`
/// the same kind of measurement taken the same way, which is the only footing on which "did it give
/// the memory back" is a fair question.
pub const MEMORY_IDLE_S: u64 = 60;
/// Percent the trailing window's two halves may differ by, and percent spread within it, before the
/// window counts as still moving. The values the shell suite used, kept so the two agree.
const MEMORY_TREND_PCT: f64 = 1.0;
const MEMORY_RANGE_PCT: f64 = 2.0;

/// Whether a steady verdict may be BELIEVED, given how the series grew since the last window.
///
/// A DEAD SAMPLER LOOKS EXACTLY LIKE A SETTLED GATEWAY, and that is the whole problem. The load loop
/// snapshots the shared series between windows and breaks the moment `plateau_check` returns `Steady`.
/// If the sampler thread dies partway through - a panic on an unexpected /proc shape for one
/// gateway's process tree is enough - the series simply stops growing. Every later snapshot is the
/// same frozen tail, and a frozen tail has zero drift and zero spread, which is the textbook
/// definition of steady. So the loop would publish "settled after N seconds" plus a peak that is
/// really "whatever was captured before the sampler died", about a gateway that may have kept
/// climbing for minutes afterwards - and the panic that caused it was thrown away by
/// `let _ = sampler.join()`, so nothing in the log or the artifact said so.
///
/// The discriminator is growth: a live sampler at ten readings a second adds samples between windows,
/// and a settled gateway still produces new samples that happen to be flat. No new samples at all is
/// not a measurement of the gateway, it is the absence of measurement.
fn steady_is_believable(samples_before: usize, samples_now: usize) -> bool {
    samples_now > samples_before
}

/// Has the series existed long enough for a steadiness verdict to MEAN anything?
///
/// `stats::window` selects by timestamp - everything at or after `last.t_s - window_s` - so a series
/// only six seconds long yields a "sixty second window" holding six seconds of data. `plateau_check`'s
/// own `n < 4` guard cannot catch that: this sampler takes ten readings a second, so six seconds is
/// sixty samples, comfortably past four.
///
/// It then judges those six seconds against `MEMORY_TREND_PCT` and `MEMORY_RANGE_PCT`, thresholds
/// chosen for a full minute, and a gateway still climbing slowly barely drifts across six seconds. So
/// the FIRST load window could come back Steady, break the loop, and publish a steady state the
/// gateway had not reached - understating its peak with a reading taken before it stopped moving.
///
/// Kept OUT of `plateau_check`, which is a general statistic with other callers and its own tests
/// asserting that four samples suffice to judge. What is specific to this loop is the decision to
/// STOP, and that is the decision that must not be made early.
///
/// Deliberately separate from `steady_is_believable`: the two answer different questions and demand
/// opposite responses. A series that stopped GROWING means the sampler is gone and the whole group is
/// void; a series that is merely too SHORT means keep measuring. Folding them together would abort a
/// perfectly healthy cell on its first window.
fn window_is_long_enough(span_s: f64) -> bool {
    span_s >= MEMORY_PLATEAU_WINDOW_S
}

/// The unsettled SHAPE as a number, because the metric surface carries `f64` and nothing else.
///
/// 1 climbing, 0 oscillating, -1 falling. Signed on purpose: the sign IS the direction, so a reader
/// or a consumer that only understands "greater than zero is bad" gets the right answer without a
/// lookup table, and the neutral shape sits at zero between the two.
fn shape_code(shape: crate::stats::Shape) -> f64 {
    match shape {
        crate::stats::Shape::Climbing => 1.0,
        crate::stats::Shape::Oscillating => 0.0,
        crate::stats::Shape::Falling => -1.0,
    }
}

/// Memory: what the gateway's process tree costs at rest and under load.
///
/// FOUR READINGS OF ONE WINDOW, which is why this is a group. Taking idle from one window and peak
/// from another would publish two populations side by side for the same gateway, the same class of
/// defect `manifest.rs` describes for a reader whose identity is not declared once.
///
/// `peak` is sampled, so it can miss a spike between polls; `hwm` is the kernel's own high-water
/// mark, updated on every charge, so it cannot. Both are published because they answer different
/// questions and disagreeing is informative.
pub struct Memory;

impl Metric for Memory {
    fn name(&self) -> &'static str {
        "memory"
    }

    fn fields(&self) -> &'static [&'static str] {
        &[
            "memory_idle_mib",
            // Whether the process was STILL or GROWING while nothing was asked of it, and the rate
            // if it grew. A leak with no load is the most damning memory result there is and the
            // single-sample idle could not see it at all.
            "memory_idle_static",
            "memory_idle_growth_rate_mib_per_min",
            // HOW each window failed to settle, when it did. See `shape_code`.
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
                // No process to measure. Every field carries the SAME reason: one cause, one
                // explanation, rather than three independently-worded absences for one fact.
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
        // METRICS runs Throughput BEFORE Memory on the same process with nothing in between, so
        // reading `idle` here without first restarting the process would read post-load RSS under
        // the name "idle": allocators do not return memory to the OS promptly, so the reading would
        // stay high and, worse, ORDER-DEPENDENT, since each cell would inherit whatever the previous
        // cell's load left resident, making the same gateway measure differently at cell 1 and cell
        // 20 and two gateways no longer comparable at all - the one thing this board exists to do.
        //
        // So the process is restarted and only then read. All four readings still come from ONE
        // window on ONE process, which is what this group is for; the window now simply starts where
        // it claims to. If the harness does not own the gateway's lifetime there is no way to return
        // it to rest, and idle is published ABSENT with that reason rather than as a number we know
        // was taken under load.
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
                // A FAILED RESTART ABORTS THE WHOLE GROUP, not just the idle reading. The old
                // behaviour marked idle absent and fell through to the sampler and the load
                // window - against a gateway in an unknown state (possibly relaunched but with
                // its post-boot configuration half-replayed), with `pid` still pointing at the
                // pre-restart tree. Every number that window produced would be the rig's own
                // failure wearing the gateway's name. HarnessError, because that is what it is.
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
                        // WATCH IT DO NOTHING, FOR AS LONG AS THE RECOVERY WINDOW WATCHES IT REST.
                        //
                        // This was one instantaneous read. A process still settling read low, which
                        // overstated every growth figure derived from idle, and a gateway that leaks
                        // with no load at all was invisible because one sample has nothing to
                        // compare against. The window is sampled at the same interval as the load
                        // window, so `idle_series` is the same shape of evidence as `rss_series`.
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
                        // The MEDIAN of the window, not its first or last reading: the same
                        // discipline `steady_state` uses, so one allocator spike cannot set the
                        // baseline every other memory figure is measured against.
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
        // KEEP THE READINGS, not just their maximum. The sampler already visits the tree every
        // MEMORY_SAMPLE_INTERVAL, so folding each reading into a running max and discarding it would
        // leave `rss_series` empty and the peak a number with no curve behind it. Whether memory
        // climbed and plateaued or spiked once is the difference between a leak and a burst, and
        // neither is visible from a single scalar.
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
                            // Sub-second precision internally. The published series carries whole
                            // seconds, but the plateau test compares the two halves of a trailing
                            // window, and at ten readings a second a truncated stamp would put them
                            // all in the same bucket and make the trend meaningless.
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
        // THE CELL'S OWN MODEL, never the bare declared one: most gateways route on the model name,
        // so a fixed model would drive this window at a different upstream than the cell it is
        // published under (run::model_for's own contract).
        let body = ctx
            .dialect
            .body(&crate::run::model_for(ctx.cfg, &ctx.id.egress));
        // The SAME headers the probe authenticated this cell with. A memory window driven with the
        // wrong credential measures a process serving 401s, which is a different workload from the
        // one every other gateway's window is compared against.
        let headers = crate::run::headers_for(ctx.cfg, ctx.dialect, &ctx.id.egress);

        // ── LOAD UNTIL IT STOPS MOVING ───────────────────────────────────────────────────────────
        //
        // Repeated windows rather than one long one, because the plateau test needs to be asked
        // between windows: a single fixed window can only ever report where memory was when the
        // clock ran out. The loop ends when the trailing minute is flat, or at the cap, and the cap
        // being reached is a RESULT (the gateway never settled) rather than a failure.
        let load_started = std::time::Instant::now();
        let mut ran = None;
        let mut verdict = crate::stats::Verdict::Undecidable;
        let mut settled_at = None;
        // How many samples the series held when it was last looked at, so a series that stops growing
        // is distinguishable from a gateway that stopped moving.
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
            // A STEADY VERDICT OFF A SERIES THAT DID NOT GROW IS THE SAMPLER'S DEATH, NOT THE
            // GATEWAY'S CALM. See `steady_is_believable`.
            let span = taken.last().map(|s| s.t_s).unwrap_or(0.0)
                - taken.first().map(|s| s.t_s).unwrap_or(0.0);
            let grew = steady_is_believable(samples_before, taken.len());
            samples_before = taken.len();
            // TOO SOON TO BELIEVE IT. Not a failure and not the gateway's answer - just a window that
            // has not lasted long enough for `plateau_check`'s thresholds to mean what they were chosen
            // to mean. Keep loading. See `window_is_long_enough`.
            if verdict.is_steady() && !window_is_long_enough(span) {
                verdict = crate::stats::Verdict::Undecidable;
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
            if verdict.is_steady() {
                settled_at = Some(load_started.elapsed().as_secs() as i64);
                break;
            }
            if load_started.elapsed().as_secs() >= MEMORY_MAX_LOAD_S {
                break;
            }
        }
        let load_s = load_started.elapsed().as_secs() as i64;

        // The kernel's high-water mark is read BEFORE the recovery window, while it still describes
        // the loaded process. It survives the load ending, but reading it here keeps it beside the
        // peak it belongs to.
        let hwm = crate::rss::hwm_tree_mib(pid);

        // ── THEN WATCH IT WITH THE LOAD GONE ─────────────────────────────────────────────────────
        //
        // The sampler is still running, so this is simply a minute of quiet appended to the same
        // series. What it shows is whether the gateway hands memory back, which a peak cannot say.
        std::thread::sleep(std::time::Duration::from_secs(MEMORY_RECOVERY_S));

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        // THE JOIN RESULT IS THE ONLY EVIDENCE THE SAMPLER PANICKED. It was discarded with `let _`,
        // which is how a sampler could die mid-window and leave a plausible, self-consistent memory
        // result behind it with nothing anywhere saying the readings had stopped. The comment a few
        // lines below already anticipated the panic ("a poisoned lock means the sampler thread
        // panicked") - it handled the consequence and threw away the cause.
        if sampler.join().is_err() {
            sampler_died = true;
            eprintln!(
                "memory: the RSS sampler thread PANICKED during this cell's load window, so the \
                 readings stop at whatever it captured before dying and no plateau verdict taken \
                 from them describes this gateway"
            );
        }

        // A DEAD SAMPLER ABORTS THE WHOLE GROUP, for the same reason a failed restart does above:
        // every number this window produced is the rig's own failure wearing the gateway's name. The
        // peak is whatever was captured before the thread died, the series stops there, and the
        // plateau verdict taken from that frozen tail describes our instrument rather than the
        // gateway. Publishing any of it - even as "settled" with a smaller peak - would be worse than
        // publishing nothing, because it is self-consistent and a reader has no way to tell.
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
            // A window that never ran means the peak was never put under load. Publishing the idle
            // reading as a peak would be a number taken under a different condition than the one it
            // claims, so it is an absence.
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

        // Take the readings back off the sampler. A poisoned lock means the sampler thread panicked,
        // which is a lost series and not a reason to lose the scalars beside it.
        let taken: Vec<crate::stats::Sample> = series.lock().map(|s| s.clone()).unwrap_or_default();

        // RECOVERED: where the curve ends, after the load has been gone for a minute. Taken from the
        // trailing recovery window rather than the single last reading, so one sample cannot set it.
        let recovered = {
            let cut = taken.last().map(|s| s.t_s).unwrap_or(0.0) - MEMORY_RECOVERY_MEDIAN_S as f64;
            let tail: Vec<f64> = taken
                .iter()
                .filter(|s| s.t_s >= cut)
                .map(|s| s.mib)
                .collect();
            crate::stats::median(&tail)
        };
        // The plateau verdict, published rather than kept. "Never settled" is a real finding about a
        // gateway and it must arrive WITH the rate it was climbing at, which is what NotSteady
        // carries; "we could not tell" stays a third, distinct answer.
        // The shape rides with the verdict. "Never settled" describes both a gateway climbing without
        // bound and one oscillating around a level it keeps returning to, and only the first is a
        // leak - publishing them under one word brands a working garbage collector as a defect.
        let mut memory_shape = Measurement::absent_because(
            Absent::NotMeasured,
            "a settled window has no unsettled shape to describe".to_string(),
        );
        let (plateaued, growth) = match &verdict {
            // STEADY PUBLISHES THE MEASURED SLOPE, NOT A ZERO. This substituted `Measured(0.0)` for the
            // rate `plateau_check` had just fitted, so a window drifting 0.9% across the minute - inside
            // `MEMORY_TREND_PCT`, hence steady - published a growth rate of exactly 0.000, and the number
            // a reader would use to spot a slow leak was a constant a threshold chose. `plateaued` is the
            // verdict and this is the measurement; the artifact publishes both, and neither is derived
            // from the other.
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
            crate::stats::Verdict::Undecidable => (
                None,
                Measurement::absent_because(
                    Absent::NotMeasured,
                    "too few readings fell inside the settle window to judge whether memory moved"
                        .to_string(),
                ),
            ),
        };
        // THE SAME PLATEAU TEST THE LOAD WINDOW USES, pointed at the idle window. Reusing it rather
        // than inventing a second rule means "still" means the same thing on both halves of the
        // curve, and a reader comparing them is comparing like with like.
        let mut idle_shape = Measurement::absent_because(
            Absent::NotMeasured,
            "a settled idle window has no unsettled shape to describe".to_string(),
        );
        let (idle_static, idle_growth) = if idle_series.len() < 2 {
            let why = format!(
                "the {MEMORY_IDLE_S}s idle window produced too few readings to say whether memory moved"
            );
            (
                Measurement::absent_because(Absent::NotMeasured, why.clone()),
                Measurement::absent_because(Absent::NotMeasured, why),
            )
        } else {
            // The verdict CARRIES the rate, so "it moved" can never be published without saying how
            // fast - that coupling is the enum's own design and this reuses it rather than computing
            // a second, independently-derived number beside it.
            match crate::stats::plateau_check(
                &idle_series,
                MEMORY_IDLE_S as f64,
                MEMORY_TREND_PCT,
                MEMORY_RANGE_PCT,
            ) {
                // Same fix as the load window above: the flag is the verdict, the rate is the
                // measurement, and a steady idle window still has a fitted slope worth publishing.
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
                crate::stats::Verdict::Undecidable => {
                    let why = format!(
                        "the {MEMORY_IDLE_S}s idle window held too few readings to judge whether memory moved"
                    );
                    (
                        Measurement::absent_because(Absent::NotMeasured, why.clone()),
                        Measurement::absent_because(Absent::NotMeasured, why),
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
                            // Reason strings ride into the board's tooltips verbatim, so this one states
                            // what the window measured and stops there - no verdict on the gateway.
                            "memory reached no steady state inside the load cap, so there is no time-to-plateau"
                                .to_string(),
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
/// Public because the CONCURRENT stream windows (`run::stream_window`, behind the two groups at the
/// bottom of this file) must read a stream exactly the way the c=1 probe here does. Two readers with
/// two budgets would measure two different stream lengths and publish the difference as a property of
/// the gateway.
pub const STREAM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
pub const STREAM_FRAME_BUDGET: usize = 64;

/// The hard ceiling on TOTAL SSE events a delivery-budgeted read will spend (`http::SseBudget`).
///
/// A read budgeted in CONTENT frames stops when the tokens arrive, so something else has to stop it
/// when they do not: a peer emitting nothing but `ping`s would otherwise be bounded only by
/// `STREAM_TIMEOUT`, and twenty seconds per lane at high concurrency is a search that never returns.
///
/// 4x the frame budget, which is 256 events for the 63 content frames an openai lane wants. That is
/// room for THREE framing events per token - far past anything a real protocol does (the mock spends
/// 3 events on openai framing and 5 on anthropic, in TOTAL, and a ping-heavy gateway adds one event
/// per keepalive interval) while still bounding the read at a fixed cost. Generous on purpose: the
/// ceiling exists to stop a pathological peer, not to judge a gateway's framing style, and a ceiling
/// tight enough to bind on a real stream would be the constant-denominator defect again in another
/// shape.
pub const STREAM_EVENT_CEILING: usize = 4 * STREAM_FRAME_BUDGET;

/// How many single-token streams the TTFT distribution is taken over, per leg.
///
/// A percentile needs samples, and one stream yields exactly one time-to-first-token - which is why
/// `added_ttft_p99_us` was absent on every cell ever published rather than measured. 100 is the
/// smallest sample where a 99th percentile is a real order statistic rather than a restatement of
/// the maximum, and it is affordable because a TTFT sample reads ONE frame and stops: milliseconds
/// each, not the ~1.3s a full paced stream takes.
pub const STREAM_TTFT_SAMPLES: usize = 100;

/// Streaming: what the gateway ADDS to a stream, rather than what the stream costs.
///
/// Every number here is a difference. The same stream is taken through the gateway and again
/// straight to the mock, and what is published is the gap between them, because the mock's own time
/// to first token is a property of the rig and would otherwise be charged to whichever gateway
/// happened to be measured on a slow box.
///
/// A dialect the MOCK cannot stream is a rig limit, not a gateway failure. `Dialect::streams_natively`
/// already records which two dialects the mock answers with real SSE frames, and its comment is
/// explicit that a dialect it returns false for must be reported as the rig being unable to pose the
/// question. Publishing a gateway as "does not stream" because our mock cannot ask is the exact
/// harness-bug-as-gateway-property inversion the project forbids.
pub struct Streaming;

impl Metric for Streaming {
    fn name(&self) -> &'static str {
        "streaming"
    }

    /// The last two are surface-only, feeding `CellStream.stream_c1_note` rather than a published
    /// number of their own, for the same reason `AddedLatency` carries its sample counts: this group
    /// takes ONE stream per leg, and how many frames each leg actually produced is the difference
    /// between a gap p50 over sixty-odd intervals and one over two. Nothing else in the artifact
    /// records it, and it comes from the same two streams the differences do.
    fn fields(&self) -> &'static [&'static str] {
        &[
            "added_ttft_p50_us",
            "added_ttft_p99_us",
            "added_gap_p50_us",
            "added_gap_p99_us",
            "gateway_c1_frames",
            "direct_c1_frames",
            // HOW MANY TTFT PROBES SURVIVED, per leg. The percentiles above are taken over whatever
            // came back out of `STREAM_TTFT_SAMPLES` attempts, and a failed probe was dropped inside a
            // `filter_map` - so a p99 over three lucky samples published identically to one over a
            // hundred, and with a single survivor the p50 and p99 ranks collapse to the same index and
            // the pair reads as coherent. `AddedLatency` publishes `gateway_c1_samples` and
            // `direct_c1_samples` for exactly this reason; the streaming group stated no weight at all.
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

        // THE HEADER SHAPE IS THE DIALECT'S, NOT A HARDCODED BEARER. This was
        // `authorization: Bearer <auth>` for every dialect, which is the wrong header NAME for two of
        // the six (anthropic sends `x-api-key` plus a mandatory version header, gemini sends
        // `x-goog-api-key`), so those two rows' streaming legs were driven unauthenticated and their
        // 401s read as the gateway not streaming. The gateway leg additionally carries whatever
        // routing headers the manifest needs to select this egress column, exactly as the probe does;
        // the direct leg carries the dialect's own auth and nothing else, because routing headers
        // select an upstream INSIDE a gateway and mean nothing to the mock.
        let gw_headers = crate::run::headers_for(ctx.cfg, ctx.dialect, &ctx.id.egress);
        let direct_headers = ctx.dialect.auth_headers(&ctx.cfg.auth);
        // The cell's own model (run::model_for): a fixed model would stream against the wrong
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
        // figures come from the sample set below, because one observation cannot carry a percentile
        // and a p50 from one stream beside a p99 from a hundred is two populations wearing one name.
        let _ = (gw_ttft, direct_ttft);

        // A TTFT DISTRIBUTION, because a column that can never hold a number should either be
        // measured or deleted.
        //
        // `added_ttft_p99_us` was absent on all 69 served cells of the 2026-07-28 run, suppressed
        // with "one stream was taken, which cannot support a 99th percentile". That was true and it
        // was not a reason to keep publishing the field: one stream yields exactly one
        // time-to-first-token, so the fix is more streams, not a better excuse.
        //
        // It is cheap, which is the part I had wrong when I called it a cost decision. A TTFT sample
        // does not need the whole stream - `post_json_sse` takes a frame budget, and a budget of ONE
        // returns on the first EVENT. Not the first token: `SseBudget::Events(1)` counts events, and a
        // dialect that opens with scaffolding (openai sends a role delta, anthropic a `message_start`)
        // satisfies it before any content arrives. What this measures is therefore time-to-first-EVENT,
        // which is the honest name for it - and it is the same quantity on both legs, so the difference
        // still isolates what the gateway added. That is milliseconds, not the ~1.3s a full
        // 64-frame paced stream takes. `STREAM_TTFT_SAMPLES` of them per leg is well under a second.
        //
        // Percentile per leg, THEN differenced - the same shape `AddedLatency` publishes for the
        // non-streaming case - so the two "added" families mean the same thing rather than two
        // things sharing a name.
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
        // LOSS IS REPORTED, not merely counted. A leg that sheds most of its probes still produces a
        // publishable percentile, so the count below is what lets a reader weigh it - and a large loss
        // is worth a line on stderr while the run is still happening.
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
        // The rank comes from `stats::nearest_rank_index`, the engine's ONE percentile convention,
        // rather than being spelled out again here. Ledger SRCH-04: this expression used to carry
        // its own ceil while `gen.rs`, `stats.rs` and `search.rs` each carried their own floor, and
        // the comments here claimed all four agreed. Over the 100 samples this leg takes they
        // disagree by a rank on every percentile whose `n * p` is a whole number.
        let ttft_pct = |v: &[u64], pct: f64| -> Option<f64> {
            if v.is_empty() {
                return None;
            }
            Some(v[crate::stats::nearest_rank_index(v.len(), pct)] as f64)
        };
        // BOTH PERCENTILES COME FROM THE SAME SAMPLES, or they are not percentiles of one thing.
        //
        // The first version of this took p99 from the sample set and left p50 as the single full
        // stream measured above. The 2026-07-28 validation run showed exactly what that produces:
        // every cell published a p99 BELOW its p50 (523/428, 514/451, 501/359), which no percentile
        // pair over one distribution can do. Two populations wearing one name.
        //
        // The sample set is the distribution now, for both. It is also the better one: 100 samples
        // per leg against the single stream the p50 used to come from.
        let added_ttft_at = |pct: f64| {
            match (ttft_pct(&gw_ttfts, pct), ttft_pct(&direct_ttfts, pct)) {
            (Some(g), Some(d)) if g >= d => Measurement::Measured(g - d),
            // A gateway cannot be faster than the upstream it proxies, so a negative difference is
            // noise - and saying so beats clamping it to a zero that claims the gateway added
            // nothing measurable. Same rule as the gap percentiles above. `BelowResolution`, not
            // `NotMeasured`: this is the comparison's best possible outcome, and the site renders
            // the two apart.
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

        // Percentile per leg, THEN difference - the same shape `AddedLatency` publishes
        // (`gateway_c1_p99_us` minus `direct_c1_p99_us`), so the streaming and non-streaming added
        // figures mean the same thing rather than two things with one name.
        // A NEGATIVE RAW DIFFERENCE IS BELOW RESOLUTION, NOT A MEASURED ZERO.
        //
        // Both legs carry the mock's ~20ms pacing, so this extracts a microsecond-scale signal by
        // differencing two ~20,000us numbers. When the gateway's own tail at a percentile lands under
        // the mock's, the raw difference is negative - physically impossible for a proxy, so it is
        // noise. Clamping that to 0 publishes "the gateway added nothing" with a precision this rig
        // does not have, and it produced incoherent pairs in the 2026-07-28 run: aisix p50=4 p99=0,
        // helicone p50=3 p99=0, plano p50=1 p99=0, tensorzero p50=1 p99=0. A p99 below its own p50
        // cannot come from one distribution.
        //
        // Absent with the reason instead. "Too small for this rig to see" is a different statement
        // from "zero", and only one of them is true.
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
            // The single-stream `added_ttft` is no longer published as the p50: it was one
            // observation, and the p99 beside it came from a hundred. It stays computed above
            // because the early-return path uses the same frames to prove a stream arrived at all.
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
            // The weight behind the two added-TTFT percentiles above. Always MEASURED, including when
            // it is zero: "no probe came back" is a fact about this cell, and an absence here would be
            // the one number a reader needs to judge the percentiles going missing itself.
            ("ttft_gw_samples", Measurement::Measured(gw_n)),
            ("ttft_direct_samples", Measurement::Measured(direct_n)),
        ];
        fields.into()
    }
}

/// Added latency: what the gateway adds to a single request's round trip at concurrency 1, over the
/// same request taken straight to the mock.
///
/// ONE PAIRED COMPARISON, TWO NUMBERS PLUS THEIR OWN RAW READINGS - which is why this is a group
/// rather than two: `added_latency_p99_us` and `gateway_c1_p99_us`/`direct_c1_p99_us` come from the
/// SAME two windows, and re-running either leg to fill a field the other group forgot would put the
/// difference and its own operands on two different populations, exactly the defect this file's
/// module doc names for peak-and-concurrency.
///
/// Concurrency 1 on purpose: throughput is what a gateway does under load, but added latency is
/// asking a narrower question - what does ONE request cost, with nothing else contending for the
/// gateway's attention - and any concurrency above 1 reintroduces queueing delay into a number that
/// is supposed to isolate the gateway's own per-request overhead.
pub struct AddedLatency;

/// Whether a c=1 window's own reading may be trusted as a leg of the added-latency comparison: it
/// produced at least one success, and did so with no failure. A window that mixed successes and
/// failures is not "what this leg costs", it is a window neither leg completed cleanly - the same
/// clean-window bar `SweepProbe` uses for a throughput rung (`fail == 0 && ok > 0`). A free function,
/// like `run::sustained_gate_passes`, so the boundary can be pinned directly without a subprocess
/// load window behind it.
fn clean_c1_leg(ok: u64, fail: u64) -> bool {
    ok > 0 && fail == 0
}

/// The added-latency difference itself: the gateway leg's reading minus the direct-to-mock leg's, at
/// microsecond resolution. A gateway cannot legitimately answer faster than the upstream it proxies -
/// a negative raw difference is rig noise (two separate processes, two separate windows, run one
/// after the other on a real box), and publishing it as a negative added latency would claim the
/// gateway returned a response before the mock itself had produced one, which a proxy cannot do.
/// `BelowResolution` rather than a clamped 0, the SAME rule as `Streaming::measure`'s `added_ttft`
/// and `added_gap`: all six published differences say "too small for this rig to see" the same way,
/// instead of two of them claiming a measured zero with a precision the rig does not have.
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

    /// The last two are NOT published as their own artifact numbers. They exist because
    /// `CellPerf.c1_note` is an advisory string about these very legs, and the only thing worth
    /// saying there is HOW MANY round trips each p99 was computed over: a p99 taken across four
    /// thousand samples and one taken across eleven are the same field with wildly different weight,
    /// and nothing else in the artifact says which this is. They ride the metric surface rather than
    /// being recomputed in `suite.rs`, because the counts belong to the SAME two windows the
    /// percentiles came from, and a second pair of windows to count them would be the two-populations
    /// defect this file's module doc names.
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

        // The cell's own model on BOTH legs (run::model_for): the gateway leg must reach this
        // cell's upstream, and the direct leg must ask the mock the same question.
        let model = crate::run::model_for(ctx.cfg, &ctx.id.egress);
        let body = ctx.dialect.body(&model);
        let gw_path = crate::run::path_for(ctx.cfg, ctx.dialect, &ctx.id.egress);
        let direct_path = ctx.dialect.mock_direct_path(&model);

        // THE SAME DURATION EVERY OTHER WINDOW IN THIS ENGINE USES, not a second magic number.
        //
        // At concurrency 1 a window's sample count is duration / round-trip-time rather than
        // duration * concurrency, so it is naturally smaller than a saturating sweep window's - but
        // `cfg.sweep_duration_s` is already sized to run a 36-cell grid across 13 gateways in a
        // reasonable box-time (6s in production, per `bin/otb.rs`'s default), and 6 seconds of
        // serial round trips against anything answering in single-digit milliseconds - every dialect
        // this rig drives - is hundreds to low thousands of samples, comfortably enough for a stable
        // p99. Reusing the constant that already governs every other window keeps one knob for "how
        // long is a load window" rather than two that can silently drift apart.
        // THE TWO LEGS AUTHENTICATE DIFFERENTLY, ON PURPOSE. The gateway leg carries everything the
        // probe carried, including whatever routing headers the manifest needs to select this egress
        // column. The direct leg carries the DIALECT's own auth shape and nothing else: routing
        // headers select an upstream INSIDE a gateway and mean nothing to the mock, exactly as
        // `mock_healthy` already reasons about its own request.
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
        // A LEG WITH ANY FAILURE IS NOT A LATENCY READING OF THAT LEG. The counts publish in the
        // detail because they ARE the finding when everything failed - the site renders "failed -
        // 0/14201 ok" off exactly this sentence - and the budget-exceeded share separates "the
        // gateway refused" from "the response outran a bound of ours".
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
            // The successful round trips behind each percentile. Both legs are already known clean
            // here (`clean_c1_leg` above returned true for each), so `ok` IS the sample count.
            ("gateway_c1_samples", Measurement::Measured(gw.ok as f64)),
            ("direct_c1_samples", Measurement::Measured(direct.ok as f64)),
        ];
        fields.into()
    }
}

/// Sustained throughput: the highest concurrency at which the gateway holds p99 under
/// `run::SUSTAINED_P99_CEILING_US` with an error rate under `run::SUSTAINED_MAX_FAIL_RATIO` (the
/// README's own gate), and the requests/sec it sustains there.
///
/// ONE BISECTION, TWO NUMBERS - the ceiling and the rate it sustains there come from the SAME search
/// for the same reason `Throughput`'s peak and its concurrency do: re-deriving the rate from a second
/// window at the winning concurrency would measure a different population than the one that proved
/// the ceiling.
///
/// A DIFFERENT SEARCH SHAPE THAN `Throughput`, which is why this is a separate group rather than a
/// third and fourth field bolted onto it. Peak throughput is unimodal (rises then falls) and is found
/// by `search::saturation_plateau`; sustained throughput is a monotone pass/fail gate in concurrency (once p99
/// blows past the ceiling it does not come back under it as concurrency keeps climbing) and is found
/// by `search::bisect_ceiling`. Conflating the two searches into one group would either run the wrong
/// algorithm for one of the two numbers or run two searches and call it one group, both of which this
/// file's module doc names as the defect a group exists to prevent.
pub struct SustainedThroughput;

// ── the two concurrent-stream groups ──────────────────────────────────────────────────────────────
//
// WHY TWO GROUPS AND NOT ONE, decided by this file's own rule: numbers from ONE search share a group,
// numbers from SEPARATE searches do not.
//
// `streams_sustained` and `streams_sustained_fps` come from one `bisect_ceiling` over a monotone
// pass/fail gate (the README's "99.9% of expected frames, no stall past 2x the pace, under 0.1%
// stream errors"), and the frames/sec is read straight off the winning rung of that same bisection -
// so they are one group for exactly the reason `SustainedThroughput`'s ceiling and rate are.
//
// `cpu_fps` and `cpu_fps_concurrency` come from a `saturation_plateau` over a saturating curve. That is a
// DIFFERENT ALGORITHM over a DIFFERENT verdict, and folding it in beside the gate would mean either
// running the wrong search for one of the four numbers or running two searches inside one group and
// calling it one - the two failure modes the module doc names. Same reasoning that already split
// `Throughput` from `SustainedThroughput`, applied to the same axis one lane over.
//
// The two groups DO share their window driver (`run::stream_window`), and that is not the same thing
// as sharing a search: sharing the instrument is what makes their numbers comparable, sharing a
// search would be what makes them one population.

/// The inter-frame gap at a percentile, over the gaps INSIDE one stream.
///
/// A stream carrying `STREAM_FRAME_BUDGET` frames yields that many gaps minus one, so a gap
/// percentile is a real distribution even from a single stream - unlike time-to-first-token, of
/// which a stream produces exactly one. Conflating those two is what left `added_gap_p99_us` absent
/// on all 69 served cells of the 2026-07-28 run while charts.py drew a chart from it.
///
/// Nearest-rank through `stats::nearest_rank_index`, the engine's single percentile convention, so a
/// published percentile is always a gap some pair of frames actually produced rather than an
/// interpolation between two that neither did - and so it means the same thing as the load
/// generator's p99 it is published beside, which it did not before ledger SRCH-04 was closed.
/// `None` when there is no gap at all: a single frame has no inter-frame time, and a zero there
/// would read as instant delivery.
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

// THE STREAM SEARCHES TAKE THE ENGINE'S FULL CEILING, like the throughput searches always did.
//
// They used to be clamped to their own lower bound and a helper relabelled anything that hit it as
// the rig's limit rather than the gateway's. Both are gone: `run::stream_window` drives one tokio
// task per lane now instead of one OS thread, so the reason for the clamp - 65536 threads being
// scheduler thrashing rather than a bigger gateway - no longer exists. Honest labelling of our own
// ceiling was the right thing while the ceiling was real; removing the ceiling is better.

/// WHICH SIDE OF THE CELL CANNOT BE STREAMED, if either.
///
/// The frames come from the MOCK, standing in for the upstream, so a cell can only be streamed when
/// BOTH ends can carry one: the ingress dialect has to be posable as a stream, and the egress
/// upstream has to answer with real SSE frames. Only openai and anthropic do
/// (`Dialect::streams_natively`, which mirrors the mock's own dispatch).
///
/// Guarding on the ingress alone - which all three stream groups did - checks the wrong end. In the
/// 2026-07-28 field run 20 served cells were ingress openai or anthropic (so the guard let them
/// through) with egress bedrock, cohere or gemini (so the mock produced no frames at all). Every
/// window came back `stream_errors == streams, frames: 0` at every concurrency from 1 to 4096, and
/// the cells published "no concurrency from 1 to 4096 passed the gate": our own rig limit, written
/// down as the gateway failing to stream. That is the harness-bug-as-gateway-property inversion this
/// module's own doc forbids, and the untestable branch to state it correctly already existed.
fn stream_blocked_by(ctx: &CellCtx<'_>) -> Option<String> {
    if !ctx.dialect.streams_natively() {
        return Some(ctx.dialect.as_str().to_string());
    }
    // An egress the mock cannot stream blocks the cell just as completely, and it is the end the
    // frames actually come from. An egress that does not parse as a dialect is left alone: the
    // measurement below will say what it found rather than this guessing on its behalf.
    match ctx.id.egress.parse::<crate::ingress::Dialect>() {
        Ok(eg) if !eg.streams_natively() => Some(eg.as_str().to_string()),
        _ => None,
    }
}

/// A dialect the mock cannot stream is a rig limit, not a gateway failure - the same fact
/// `Streaming::measure` opens with, and it must be stated identically here or the same rig limit
/// would be published two different ways in one cell.
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
        // Mirrors the rate's reason rather than inventing a second one, exactly as `Throughput` and
        // `SustainedThroughput` do for their own concurrency fields.
        let conc = match found.concurrency.value() {
            Some(c) => Measurement::Measured(f64::from(*c)),
            None => Measurement::absent(found.fps.reason().cloned().unwrap_or(Absent::NotMeasured)),
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

/// CPU frames/sec: the most frames a second this box carries through the gateway, and the stream
/// concurrency it peaked at.
pub struct CpuFps;

impl Metric for CpuFps {
    fn name(&self) -> &'static str {
        "cpu_fps"
    }

    fn fields(&self) -> &'static [&'static str] {
        &["cpu_fps", "cpu_fps_concurrency"]
    }

    fn measure(&self, ctx: &CellCtx<'_>) -> Measured {
        if let Some(side) = stream_blocked_by(ctx) {
            let m = stream_untestable_named(&side);
            let f: Filled = self.fields().iter().map(|x| (*x, m.clone())).collect();
            return f.into();
        }
        let found = crate::run::sweep_cpu_fps_cell(ctx.cfg, ctx.id, ctx.min_conc, ctx.max_conc);
        let carry = |m: &Measurement<f64>| match (m.reason().cloned(), m.detail()) {
            (Some(r), Some(d)) => Measurement::absent_because(r, d),
            (Some(r), None) => Measurement::absent(r),
            (None, _) => Measurement::absent(Absent::NotMeasured),
        };
        let fps = match found.fps.value() {
            Some(v) => Measurement::Measured(*v),
            None => carry(&found.fps),
        };
        let conc = match found.concurrency.value() {
            Some(c) => Measurement::Measured(f64::from(*c)),
            None => Measurement::absent(fps.reason().cloned().unwrap_or(Absent::NotMeasured)),
        };
        Measured {
            fields: vec![("cpu_fps", fps), ("cpu_fps_concurrency", conc)],
            series: Series {
                sweep_cpu_fps: found
                    .points
                    .iter()
                    .map(crate::run::StreamPoint::to_json)
                    .collect(),
                ..Series::default()
            },
        }
    }
}

#[cfg(test)]
mod tests {
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
                    rps: Measurement::Measured(10),
                    p99_us: Measurement::Measured(20),
                    fail: Measurement::Measured(0),
                };
                Measured {
                    fields: vec![],
                    series: Series {
                        sweep: vec![pt()],
                        sweep_sustained: vec![pt()],
                        rss: rss.clone(),
                        idle_rss: rss,
                        sweep_streams: vec![serde_json::Value::Null],
                        sweep_cpu_fps: vec![serde_json::Value::Null],
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
        assert!(
            !series.sweep_cpu_fps.is_empty(),
            "sweep_cpu_fps was dropped"
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

    /// Every group must declare at least one field, or it is a procedure with no way to be observed.
    #[test]
    fn every_group_declares_what_it_fills() {
        for m in METRICS {
            assert!(!m.fields().is_empty(), "{} declares no fields", m.name());
            assert!(!m.name().is_empty());
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

    // ── the reachability list itself carries the two new groups ────────────────────────────────
    //
    // `no_two_groups_claim_the_same_artifact_field` and `every_group_declares_what_it_fills` above
    // already run over `METRICS`, so adding `AddedLatency`/`SustainedThroughput` to that list is
    // what makes them reachable per this file's own module doc ("a metric is in `METRICS` or it does
    // not exist"). This test names the two fields those tests would silently miss if a future edit
    // dropped either struct back out of the list without anything else failing.
    // ── the two concurrent-stream groups ────────────────────────────────────────────────────────

    // A DIALECT THE MOCK CANNOT STREAM IS A RIG LIMIT, and it must be stated the same way in all
    // three streaming groups. `Untestable`, not `NotMeasured`: the first says the rig cannot pose the
    // question, the second says we asked and got nothing, and only one of those is true here.
    // Publishing "this gateway sustains no streams" because our own mock does not synthesise gemini
    // frames is the harness-bug-as-gateway-property inversion this project forbids.
    #[test]
    fn a_dialect_the_mock_cannot_stream_is_untestable_in_every_stream_group() {
        let cfg = a_config();
        let id = CellId::new("gemini", "gemini");
        let ctx = CellCtx {
            cfg: &cfg,
            id: &id,
            dialect: Dialect::Gemini,
            min_conc: 1,
            max_conc: 2,
        };
        assert!(
            !Dialect::Gemini.streams_natively(),
            "this test is about a dialect the mock cannot stream"
        );
        for (name, produced) in [
            ("streams_sustained", StreamsSustained.measure(&ctx)),
            ("cpu_fps", CpuFps.measure(&ctx)),
        ] {
            let filled: BTreeMap<_, _> = produced.fields.into_iter().collect();
            assert!(
                !filled.is_empty(),
                "{name} must still fill the fields it declares"
            );
            for (field, m) in &filled {
                assert_eq!(
                    m.copied(),
                    None,
                    "{name}/{field} cannot have measured anything"
                );
                assert_eq!(
                    m.reason(),
                    Some(&Absent::Untestable),
                    "{name}/{field} must name the RIG's limit, not ours"
                );
                assert!(
                    m.detail().unwrap_or_default().contains("gemini"),
                    "{name}/{field} must say which dialect the mock cannot stream: {:?}",
                    m.detail()
                );
            }
            // And no rung was probed at all, because the search never ran: a sweep trace here would
            // be evidence for a measurement that was never taken.
            assert!(filled.len() >= 2);
        }
    }

    // A group that declines to measure must still fill EVERY field it declared, which is what stops a
    // silently missing key from being indistinguishable from an unmeasured one.
    #[test]
    fn both_stream_groups_fill_every_declared_field_even_when_untestable() {
        let cfg = a_config();
        let id = CellId::new("cohere", "cohere");
        let ctx = CellCtx {
            cfg: &cfg,
            id: &id,
            dialect: Dialect::Cohere,
            min_conc: 1,
            max_conc: 2,
        };
        for m in [&StreamsSustained as &dyn Metric, &CpuFps] {
            let filled: BTreeMap<_, _> = m.measure(&ctx).fields.into_iter().collect();
            for f in m.fields() {
                assert!(
                    filled.contains_key(f),
                    "{} declares {f} and did not fill it",
                    m.name()
                );
            }
        }
    }

    /// THE FIELDS ARE WHAT MUST BE REACHABLE, NOT THE GROUP THAT HAPPENS TO OWN THEM.
    ///
    /// This used to also assert a group literally named `sustained_throughput` was in `METRICS`,
    /// and that assertion failed the moment the sustained figure moved into `throughput` - where it
    /// belongs, since it is now a summary of the same sweep rather than a search of its own. A test
    /// that breaks when a field changes hands is testing the file layout; the property worth holding
    /// is that no declared artifact field can quietly stop being produced by anyone.
    #[test]
    fn every_published_field_is_reachable_from_metrics() {
        let names: Vec<&str> = METRICS.iter().map(|m| m.name()).collect();
        assert!(names.contains(&"added_latency"), "METRICS = {names:?}");
        let all_fields: Vec<&str> = METRICS
            .iter()
            .flat_map(|m| m.fields().iter().copied())
            .collect();
        for f in [
            "added_latency_p50_us",
            "added_latency_p99_us",
            "gateway_c1_p99_us",
            "direct_c1_p99_us",
            "rps_max_proxy",
            "conc_at_peak",
            "rps_sustained_20ms",
            "rps_sustained_20ms_concurrency",
            "conc_at_sustained",
        ] {
            assert!(
                all_fields.contains(&f),
                "{f} is not declared by any group in METRICS: {all_fields:?}"
            );
        }
    }

    // The same gate for the two concurrent-stream groups. `CellStream` declared these four fields and
    // NOTHING in the engine ever filled them: the artifact carried the keys, always null, on every
    // cell of every gateway ever published. A group that falls back out of `METRICS` returns the
    // board to exactly that state with nothing else failing, which is what this holds.
    #[test]
    fn the_two_stream_groups_are_reachable_from_metrics() {
        let names: Vec<&str> = METRICS.iter().map(|m| m.name()).collect();
        assert!(names.contains(&"streams_sustained"), "METRICS = {names:?}");
        assert!(names.contains(&"cpu_fps"), "METRICS = {names:?}");
        let all_fields: Vec<&str> = METRICS
            .iter()
            .flat_map(|m| m.fields().iter().copied())
            .collect();
        for f in [
            "streams_sustained",
            "streams_sustained_fps",
            "cpu_fps",
            "cpu_fps_concurrency",
        ] {
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
        // restatement of the maximum: nearest-rank puts it at index 99 of 100, not at the top.
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
        let (_, _, all) = process_cell_with(&ctx, METRICS);
        assert_eq!(all.len(), METRICS.len(), "every group must report: {all:?}");
        for m in METRICS {
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

        // The differencing keeps that ordering: both legs are percentiles of their own sample set at
        // the same rank, so a gateway that adds a constant adds it at every percentile.
        let direct: Vec<u64> = samples.iter().map(|v| v / 2).collect();
        let add = |p: f64| (pct(&samples, p) - pct(&direct, p)).max(0.0);
        assert!(
            add(0.99) >= add(0.50),
            "the ADDED figures must hold the same ordering"
        );

        // One sample cannot support a p99 that means anything: it is that sample, and it equals the
        // p50 rather than sitting below it.
        assert_eq!(pct(&[500], 0.99), pct(&[500], 0.50));
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

    // A FABRICATED ZERO IN PUBLISHED EVIDENCE, on the artifact side of the same defect.
    //
    // `SustainedPoint.fail` was an `i64` and this mapping was `Measurement::Measured(pt.fail)`
    // unconditionally, so a rung whose windows all came back without a reading published `fail: 0`
    // in `sweep_sustained_20ms` - a row stating the gateway lost nothing at a rate nothing ever
    // observed it serving. The board's rule is that an absent measurement publishes null WITH A
    // REASON and is never substituted by a number, so the absence has to carry one.
    #[test]
    fn a_rung_with_no_reading_publishes_an_absent_failure_count_with_its_reason() {
        let pt = |conc: u32, p99: Option<u64>, fail: Option<i64>| crate::run::SustainedPoint {
            concurrency: conc,
            passed: true,
            rps: 16_000.0,
            p99_us: p99,
            fail,
        };
        let rows = sustained_evidence(&[pt(64, Some(5_000), Some(3)), pt(128, None, None)]);

        // A measured rung still publishes its count, including a real zero - "measured no failures"
        // is a fact, and it must not be collateral damage of making the absent case honest.
        assert_eq!(rows[0].fail.value().copied(), Some(3));
        assert_eq!(
            sustained_evidence(&[pt(8, None, Some(0))])[0]
                .fail
                .value()
                .copied(),
            Some(0),
            "a window that measured zero failures measured something"
        );

        // The absent one is absent, and says why at the rung it happened on.
        assert_eq!(rows[1].fail.value(), None);
        assert_eq!(rows[1].fail.reason(), Some(&Absent::NotMeasured));
        let detail = rows[1].fail.detail().unwrap_or_default();
        assert!(
            detail.contains("c=128") && detail.contains("no window"),
            "the absence must name the rung it happened on: {detail:?}"
        );
    }
}
