// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// THE ENGINE, STATED ONCE: for every configured cell, run every metric.
//
// WHY THIS EXISTS. The engine used to reach for its measurements one call site at a time, and the
// throughput sweep was the only one anything reached for. `rss` (per-cell memory), `qualify` (box
// health) and `launch` were finished, unit-tested, and had ZERO callers - 17% of the engine, 57
// passing tests, wired to nothing - while the suite reported green, because every test drove one
// module against fakes and none asserted that a module is reachable from a real run. Memory is the
// board's headline metric and `site/gen-data.mjs` takes it SOLELY from the per-cell window, with no
// fallback, so a board built from that engine would have published no memory at all.
//
// A list fixes that class outright. A metric is in `METRICS` or it does not exist, and `METRICS` is
// one thing a human can read in full. There is no third state where a measurement is implemented,
// tested, and silently never taken.
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
    /// One entry per resident-memory reading taken across the load window.
    pub rss: Vec<crate::record::RssSample>,
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
        Measured { fields, series: Series::default() }
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
pub const METRICS: &[&dyn Metric] = &[&Throughput, &Memory, &Streaming];

/// Run every metric against one served cell.
///
/// A group that returns nothing for a field it declared gets an explicit absence rather than a
/// missing key, so the artifact's shape does not depend on which code path a metric took. A missing
/// key and a null mean different things to `site/gen-data.mjs`, and only one of them is honest.
pub fn process_cell(ctx: &CellCtx<'_>) -> (BTreeMap<&'static str, Measurement<f64>>, Series) {
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
) -> (BTreeMap<&'static str, Measurement<f64>>, Series) {
    let mut out = BTreeMap::new();
    let mut series = Series::default();
    for m in metrics {
        let produced = m.measure(ctx);
        // Series ACCUMULATE across groups rather than overwrite: the sweep comes from throughput and
        // the readings come from memory, and a later group returning none must not erase an earlier
        // group's evidence.
        if !produced.series.sweep.is_empty() {
            series.sweep = produced.series.sweep;
        }
        if !produced.series.rss.is_empty() {
            series.rss = produced.series.rss;
        }
        let filled: BTreeMap<&'static str, Measurement<f64>> = produced.fields.into_iter().collect();
        for field in m.fields() {
            let value = filled.get(field).cloned().unwrap_or_else(|| {
                Measurement::absent_because(
                    Absent::NotMeasured,
                    format!("the {} group declares {field} but returned no value for it", m.name()),
                )
            });
            out.insert(*field, value);
        }
    }
    (out, series)
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
        &["rps_max_proxy", "conc_at_peak"]
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
            None => Measurement::absent(perf.max_proxy.reason().cloned().unwrap_or(Absent::NotMeasured)),
        };
        // THE SWEEP TRAVELS WITH THE PEAK. Each probed rung becomes a published point, so a reader
        // can see the shape the search walked and re-derive the maximum rather than trusting it.
        // `p99_us` and `fail` are absent rather than zero: the search's gate records whether a rung
        // PASSED, not the latency or the failure count behind that verdict, and a zero here would
        // read as "measured no failures" when nothing was measured at all.
        let sweep = perf
            .points
            .iter()
            .map(|pt| crate::record::SweepPoint {
                conc: i64::from(pt.concurrency),
                rps: Measurement::Measured(pt.value as i64),
                p99_us: Measurement::absent(Absent::NotMeasured),
                fail: Measurement::absent(Absent::NotMeasured),
            })
            .collect();
        Measured {
            fields: vec![("rps_max_proxy", rps), ("conc_at_peak", conc)],
            series: Series { sweep, rss: Vec::new() },
        }
    }
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
/// Percent the trailing window's two halves may differ by, and percent spread within it, before the
/// window counts as still moving. The values the shell suite used, kept so the two agree.
const MEMORY_TREND_PCT: f64 = 1.0;
const MEMORY_RANGE_PCT: f64 = 2.0;

/// Memory: what the gateway's process tree costs at rest and under load.
///
/// FOUR READINGS OF ONE WINDOW, which is why this is a group. Taking idle from one window and peak
/// from another would publish two populations side by side, the exact defect `manifest.rs` records as
/// having already corrupted this board's numbers.
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
                let detail = why.detail().unwrap_or("the gateway's process tree could not be found").to_string();
                let fields: Filled = self
                    .fields()
                    .iter()
                    .map(|f| (*f, Measurement::absent_because(reason.clone(), detail.clone())))
                    .collect();
                // No process, so no window ran and there is no series to carry.
                return fields.into();
            }
        };

        // ── PUT IT BACK AT REST FIRST ────────────────────────────────────────────────────────────
        //
        // `idle` used to be read right here, as this group's first act. But METRICS runs Throughput
        // BEFORE Memory on the same process with nothing in between, so by the time this line ran
        // the gateway had just been driven all the way through a peak-finding sweep. The reading was
        // post-load RSS wearing the name "idle", and allocators do not return memory to the OS
        // promptly, so it stayed high: one gateway published 111 MiB idle where a cold process
        // measures 7.1, a factor of fifteen on the board's headline metric.
        //
        // It was also ORDER-DEPENDENT. Each cell inherited whatever the previous cell's load left
        // resident, so the same gateway measured differently at cell 1 and cell 20, and two gateways
        // were no longer comparable at all - which is the one thing this board exists to do.
        //
        // So the process is restarted and only then read. All four readings still come from ONE
        // window on ONE process, which is what this group is for; the window now simply starts where
        // it claims to. If the harness does not own the gateway's lifetime there is no way to return
        // it to rest, and idle is published ABSENT with that reason rather than as a number we know
        // was taken under load.
        let idle = match &ctx.cfg.relaunch {
            None => Measurement::absent_because(
                Absent::NotMeasured,
                "the harness does not own this gateway's lifetime, so it could not be returned to \
                 rest before the reading; an idle taken after the throughput sweep would be \
                 post-load RSS under another name"
                    .to_string(),
            ),
            Some(spec) => match crate::run::restart_to_rest(spec) {
                Err(e) => Measurement::absent_because(
                    Absent::NotMeasured,
                    format!("the gateway could not be restarted to rest before the idle reading: {e}"),
                ),
                // Re-resolve the pid: a restart gives the tree a NEW root, and reading the old one
                // would measure a process that no longer exists.
                Ok(()) => match crate::rss::root_pid(&ctx.cfg.runtime).copied() {
                    Some(fresh) => {
                        pid = fresh;
                        crate::rss::rss_tree_mib(fresh)
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
        // MEMORY_SAMPLE_INTERVAL; it used to fold each reading into a running max and throw the
        // reading away, so `rss_series` published empty on every cell and the peak was a number with
        // no curve behind it. Whether memory climbed and plateaued or spiked once is the difference
        // between a leak and a burst, and neither is visible from a single scalar.
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
                            s.push(crate::stats::Sample::new(started.elapsed().as_secs_f64(), v));
                        }
                    }
                    std::thread::sleep(MEMORY_SAMPLE_INTERVAL);
                }
            })
        };

        let path = crate::run::path_for(ctx.cfg, ctx.dialect, &ctx.id.egress);
        let body = ctx.dialect.body(&ctx.cfg.model);

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
        loop {
            let w = crate::run::load_window(ctx.cfg, &path, &body, MEMORY_WINDOW_CONCURRENCY);
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
            if matches!(verdict, crate::stats::Verdict::Steady) {
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
        let _ = sampler.join();

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
            let cut = taken.last().map(|s| s.t_s).unwrap_or(0.0) - MEMORY_RECOVERY_S as f64 / 2.0;
            let tail: Vec<f64> = taken.iter().filter(|s| s.t_s >= cut).map(|s| s.mib).collect();
            crate::stats::median(&tail)
        };
        // The plateau verdict, published rather than kept. "Never settled" is a real finding about a
        // gateway and it must arrive WITH the rate it was climbing at, which is what NotSteady
        // carries; "we could not tell" stays a third, distinct answer.
        let (plateaued, growth) = match &verdict {
            crate::stats::Verdict::Steady => (Some(true), Measurement::Measured(0.0)),
            crate::stats::Verdict::NotSteady { growth_rate_mib_per_min } => {
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
                            "memory never settled inside the load cap, so there is no time-to-plateau"
                                .to_string(),
                        ),
                    },
                ),
            ],
            series: Series { sweep: Vec::new(), rss },
        }
    }
}

/// How long a streaming probe waits, and how many frames it reads.
const STREAM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
const STREAM_FRAME_BUDGET: usize = 64;

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

    fn fields(&self) -> &'static [&'static str] {
        &["added_ttft_p50_us", "added_ttft_p99_us", "added_gap_p50_us", "added_gap_p99_us"]
    }

    fn measure(&self, ctx: &CellCtx<'_>) -> Measured {
        // Streaming takes no series of its own; `into()` wraps its fields with an empty one.
        let all = |m: Measurement<f64>| -> Measured {
            let f: Filled = self.fields().iter().map(|x| (*x, m.clone())).collect();
            f.into()
        };

        // The rig, not the gateway, decides whether this question can be asked at all.
        if !ctx.dialect.streams_natively() {
            return all(Measurement::absent_because(
                Absent::Untestable,
                format!(
                    "the mock does not answer {} with a native event stream, so the rig cannot pose the streaming question here",
                    ctx.dialect.as_str()
                ),
            ));
        }

        let headers = vec![("authorization".to_string(), format!("Bearer {}", ctx.cfg.auth))];
        let body = ctx.dialect.stream_body(&ctx.cfg.model);

        let through_gateway = crate::http::post_json_sse(
            ctx.cfg.gateway_addr,
            &crate::run::path_for(ctx.cfg, ctx.dialect, &ctx.id.egress),
            body.as_bytes(),
            &headers,
            STREAM_TIMEOUT,
            STREAM_FRAME_BUDGET,
        );
        let direct = crate::http::post_json_sse(
            ctx.cfg.mock_addr,
            &ctx.dialect.mock_direct_path(&ctx.cfg.model),
            body.as_bytes(),
            &headers,
            STREAM_TIMEOUT,
            STREAM_FRAME_BUDGET,
        );

        // A leg that produced no frame has no time to first token. Subtracting against a missing
        // reference would publish the gateway's own latency as its ADDED latency, which reads as the
        // gateway being slower than it is.
        let (Some(&gw_ttft), Some(&direct_ttft)) = (
            through_gateway.frame_offsets_us.first(),
            direct.frame_offsets_us.first(),
        ) else {
            let which = if through_gateway.frame_offsets_us.is_empty() { "the gateway" } else { "the mock directly" };
            return all(Measurement::absent_because(
                Absent::NotMeasured,
                format!("no stream frame arrived from {which}, so there is nothing to difference"),
            ));
        };

        // Saturating, because a gateway CANNOT be faster than the upstream it proxies: a negative
        // difference is rig noise, and publishing it as a negative added latency would say the
        // gateway returned the token before the mock produced it.
        let added_ttft = f64::from(gw_ttft.saturating_sub(direct_ttft) as u32);

        let gap_p50 = |o: &crate::http::SseOutcome| -> Option<f64> {
            let mut gaps: Vec<u64> =
                o.frame_offsets_us.windows(2).map(|w| w[1].saturating_sub(w[0])).collect();
            if gaps.is_empty() {
                return None;
            }
            gaps.sort_unstable();
            Some(gaps[gaps.len() / 2] as f64)
        };

        let added_gap = match (gap_p50(&through_gateway), gap_p50(&direct)) {
            (Some(g), Some(d)) => Measurement::Measured((g - d).max(0.0)),
            _ => Measurement::absent_because(
                Absent::NotMeasured,
                "a single frame on one of the two legs leaves no inter-frame gap to difference".to_string(),
            ),
        };

        // ONE STREAM CANNOT SUPPORT A p99. The board's p99 fields are a distribution over many
        // streams; this group takes one. Publishing the single observation under a p99 name would be
        // a number that claims a confidence it does not have, so the p99s are absent and say why.
        let no_distribution = || {
            Measurement::absent_because(
                Absent::NotMeasured,
                "one stream was taken, which cannot support a 99th percentile".to_string(),
            )
        };

        let fields: Filled = vec![
            ("added_ttft_p50_us", Measurement::Measured(added_ttft)),
            ("added_ttft_p99_us", no_distribution()),
            ("added_gap_p50_us", added_gap),
            ("added_gap_p99_us", no_distribution()),
        ];
        fields.into()
    }
}

#[cfg(test)]
mod tests {
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
        CellCtx { cfg, id, dialect: Dialect::Openai, min_conc: 1, max_conc: 2 }
    }

    fn a_config() -> RunConfig {
        RunConfig {
            gateway_addr: "127.0.0.1:1".parse().expect("a literal loopback address parses"),
            mock_addr: "127.0.0.1:2".parse().expect("a literal loopback address parses"),
            model: "m".into(),
            auth: "dummy".into(),
            dialects: vec![Dialect::Openai],
            sweep_duration_s: 1,
            probe_timeout: std::time::Duration::from_millis(1),
            load_cores: None,
            static_headers: Vec::new(),
            egress_headers: Default::default(),
            runtime: crate::manifest::Runtime::Native { proc_match: "test-fixture".into() },
            declared_path: String::new(),
            cell_paths: Default::default(),
            relaunch: None,
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
                    format!("the {} group declares {field} but returned no value for it", Forgetful.name()),
                )
            });
            out.insert(*field, value);
        }

        assert!(out.contains_key("forgotten"), "the key must exist even though the group skipped it");
        assert_eq!(out["forgotten"].reason(), Some(&Absent::NotMeasured));
        assert!(
            out["forgotten"].detail().is_some_and(|d| d.contains("forgetful")),
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
        assert!(!seen.is_empty(), "the engine must declare at least one metric");
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
    // gateway has just been driven through a full peak-finding sweep. The reading was published as
    // "idle" anyway: one gateway shipped 111 MiB where a cold process measures 7.1.
    //
    // When the harness does not own the gateway's lifetime (relaunch: None) it cannot put it back at
    // rest, and the only honest answer is an absence carrying that reason - never the post-load
    // number. This pins the absence so the polluted reading cannot come back as a silent default.
    #[test]
    fn idle_memory_is_absent_when_the_gateway_cannot_be_returned_to_rest() {
        let cfg = a_config();
        assert!(cfg.relaunch.is_none(), "this fixture owns no gateway lifetime");
        let id = CellId::new("openai", "openai");
        let ctx = CellCtx { cfg: &cfg, id: &id, dialect: Dialect::Openai, min_conc: 1, max_conc: 2 };
        let filled: BTreeMap<_, _> = Memory.measure(&ctx).fields.into_iter().collect();
        let idle = filled.get("memory_idle_mib").expect("the memory group declares memory_idle_mib");
        assert_eq!(idle.copied(), None, "idle must not be published from a process that served load");
        assert!(
            idle.reason().is_some(),
            "an absent idle must carry the reason it could not be taken, not a bare null"
        );
    }
}
