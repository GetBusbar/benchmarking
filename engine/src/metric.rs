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
    fn measure(&self, ctx: &CellCtx<'_>) -> Filled;
}

/// THE ENGINE'S ENTIRE MEASUREMENT SURFACE.
///
/// Adding a number to the board is: implement a group, add it here. Removing one is deleting it from
/// this list, which is a visible act rather than a call that quietly stopped happening.
pub const METRICS: &[&dyn Metric] = &[&Throughput, &Memory];

/// Run every metric against one served cell.
///
/// A group that returns nothing for a field it declared gets an explicit absence rather than a
/// missing key, so the artifact's shape does not depend on which code path a metric took. A missing
/// key and a null mean different things to `site/gen-data.mjs`, and only one of them is honest.
pub fn process_cell(ctx: &CellCtx<'_>) -> BTreeMap<&'static str, Measurement<f64>> {
    let mut out = BTreeMap::new();
    for m in METRICS {
        let filled: BTreeMap<&'static str, Measurement<f64>> = m.measure(ctx).into_iter().collect();
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
    out
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

    fn measure(&self, ctx: &CellCtx<'_>) -> Filled {
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
        vec![("rps_max_proxy", rps), ("conc_at_peak", conc)]
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
        &["memory_idle_mib", "memory_peak_mib", "memory_hwm_mib"]
    }

    fn measure(&self, ctx: &CellCtx<'_>) -> Filled {
        // The tree to measure comes from the ONE declared identity, the same one the launcher's
        // --name and the stop path use.
        let pid = match crate::rss::root_pid(&ctx.cfg.runtime).copied() {
            Some(p) => p,
            None => {
                // No process to measure. Every field carries the SAME reason: one cause, one
                // explanation, rather than three independently-worded absences for one fact.
                let why = crate::rss::root_pid(&ctx.cfg.runtime);
                let reason = why.reason().cloned().unwrap_or(Absent::NotMeasured);
                let detail = why.detail().unwrap_or("the gateway's process tree could not be found").to_string();
                return self
                    .fields()
                    .iter()
                    .map(|f| (*f, Measurement::absent_because(reason.clone(), detail.clone())))
                    .collect();
            }
        };

        let idle = crate::rss::rss_tree_mib(pid);

        // Sample the tree while a window of load runs against it. The sampler is a plain thread
        // rather than a timer: it stops when the window's child exits, so a slow window is sampled
        // for as long as it actually ran instead of for as long as it was expected to.
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let peak_seen = std::sync::Arc::new(std::sync::Mutex::new(f64::NEG_INFINITY));
        let sampler = {
            let stop = std::sync::Arc::clone(&stop);
            let peak_seen = std::sync::Arc::clone(&peak_seen);
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Some(v) = crate::rss::rss_tree_mib(pid).copied() {
                        if let Ok(mut p) = peak_seen.lock() {
                            *p = p.max(v);
                        }
                    }
                    std::thread::sleep(MEMORY_SAMPLE_INTERVAL);
                }
            })
        };

        let path = ctx.dialect.path(&ctx.cfg.model);
        let body = ctx.dialect.body(&ctx.cfg.model);
        let ran = crate::run::load_window(ctx.cfg, &path, &body, MEMORY_WINDOW_CONCURRENCY);

        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = sampler.join();

        // The kernel's high-water mark is read AFTER the window: it survives the load ending, which
        // is exactly why it is trustworthy where a poll is not.
        let hwm = crate::rss::hwm_tree_mib(pid);

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

        vec![("memory_idle_mib", idle), ("memory_peak_mib", peak), ("memory_hwm_mib", hwm)]
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
        fn measure(&self, _ctx: &CellCtx<'_>) -> Filled {
            vec![("present", Measurement::Measured(1.0))]
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
            runtime: crate::manifest::Runtime::Native { proc_match: "test-fixture".into() },
        }
    }

    #[test]
    fn a_declared_field_that_a_group_does_not_return_becomes_an_absence_not_a_missing_key() {
        let cfg = a_config();
        let id = CellId::new("openai", "openai");
        let ctx = ctx_for(&cfg, &id);

        let filled: BTreeMap<&'static str, Measurement<f64>> = Forgetful.measure(&ctx).into_iter().collect();
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
}
