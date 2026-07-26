// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The suite driver: what matrix/run.sh did, in Rust.
//
// Walks the protocol grid, probes each cell for what the gateway ACTUALLY serves, sweeps the served
// ones, and writes one snapshot. Nothing here decides anything: every verdict comes from the modules
// that already own it, so this file is sequencing and nothing else.

use std::net::SocketAddr;
use std::time::Duration;

use crate::cell::{CellId, CellOutcome, Served};
use crate::gen::GenStats;
use crate::http::{self, Outcome};
use crate::ingress::Dialect;
use crate::measurement::{Absent, Measurement};
use crate::probe::{persistent_transient_verdict, Observation, Verdict};
use crate::metric;
use crate::search::{self, Probe, Sample};

pub struct RunConfig {
    pub gateway_addr: SocketAddr,
    pub mock_addr: SocketAddr,
    pub model: String,
    pub auth: String,
    /// Dialects to walk. Both axes use the same set: ingress is what the client speaks, egress is
    /// what the upstream speaks.
    pub dialects: Vec<Dialect>,
    pub sweep_duration_s: u64,
    pub probe_timeout: Duration,
    /// CPU list the load generator is pinned to, e.g. "4-9". None only in tests.
    pub load_cores: Option<String>,
    /// Headers this gateway needs on every request, whatever the cell.
    pub static_headers: Vec<(String, String)>,
    /// Headers that select an egress column, keyed by dialect. Empty for a gateway that routes by
    /// config rather than by header.
    pub egress_headers: std::collections::BTreeMap<String, Vec<(String, String)>>,
    /// The gateway's declared identity, so the memory readers can find its process tree. The SAME
    /// value the launcher's --name and the stop path take: there is no second name for a reader to
    /// disagree with.
    pub runtime: crate::manifest::Runtime,
    /// THE INGRESS PATH THIS GATEWAY DECLARES, when it is not the dialect's standard one.
    ///
    /// Most gateways serve the OpenAI API at `/v1/chat/completions`. Some mount their compatible
    /// API under a prefix, and one entrant declares `/openai/v1/chat/completions` in its manifest.
    /// The probe ignored that field and used the standard path, so every cell answered a truthful
    /// 404 and the artifact published the gateway as serving nothing at all. That is a false claim
    /// about somebody's product, produced entirely by us, and it is the worst class of error this
    /// board can make.
    ///
    /// Applies to the ONE dialect whose standard path it ends with; every other dialect keeps its
    /// own. A gateway that serves a dialect somewhere unusual says so, and one that does not serve
    /// it at all still answers 404, which is the honest verdict rather than an artefact of ours.
    pub declared_path: String,
    /// Per-cell overrides, keyed `"<ingress>>egress"`. See `Manifest::cell_paths`.
    pub cell_paths: std::collections::BTreeMap<String, String>,
    /// HOW TO PUT THE GATEWAY BACK AT REST. The memory group needs a process that has not served
    /// load to read an idle RSS from, and the only way to get one is to restart it, so the spec that
    /// launched it has to be reachable from a metric.
    ///
    /// `None` when the harness does not own the gateway's lifetime (no `launch` in the manifest, or
    /// a run against an already-up target). The memory group then publishes idle as ABSENT rather
    /// than as a reading it knows was taken under load - see `Memory::measure`.
    pub relaunch: Option<crate::launch::LaunchSpec>,
}

/// Every header one request carries: how this INGRESS dialect authenticates, then whatever the
/// gateway needs to select this EGRESS column.
///
/// Two axes, and they are genuinely different things. The auth header belongs to the protocol the
/// client is speaking and is identical across gateways, so it comes from `Dialect`. The routing
/// header belongs to the gateway and is how some of them decide which upstream to call, so it comes
/// from the manifest, keyed by column. Collapsing them into one hardcoded shape is what sent
/// `authorization: Bearer` to dialects that do not use one.
/// Where to send this dialect's probe: the gateway's declared path when it is a longer form of this
/// dialect's standard one, otherwise the standard.
pub fn path_for(cfg: &RunConfig, ingress: Dialect, egress: &str) -> String {
    let standard = ingress.path(&cfg.model);
    // Most specific first: a cell's own path, then the gateway's declared one, then the standard.
    if let Some(p) = cfg.cell_paths.get(&format!("{}>{}", ingress.as_str(), egress)) {
        return p.clone();
    }
    if !cfg.declared_path.is_empty()
        && cfg.declared_path != standard
        && cfg.declared_path.ends_with(&standard)
    {
        return cfg.declared_path.clone();
    }
    standard
}

fn headers_for(cfg: &RunConfig, ingress: Dialect, egress: &str) -> Vec<(String, String)> {
    let mut out = ingress.auth_headers(&cfg.auth);
    out.extend(cfg.static_headers.iter().cloned());
    if let Some(extra) = cfg.egress_headers.get(egress) {
        out.extend(extra.iter().cloned());
    }
    out
}

/// Ask the gateway whether it serves this pairing. The answer comes only from what was OBSERVED:
/// a real status with a healthy rig is the gateway's own answer, no HTTP answer at all is not.
pub fn probe_cell(cfg: &RunConfig, id: &CellId, mock_healthy: bool) -> Served {
    let Ok(ing) = id.ingress.parse::<Dialect>() else {
        return Served::Untestable(format!("unknown ingress dialect {}", id.ingress));
    };
    let path = path_for(cfg, ing, &id.egress);
    let body = ing.body(&cfg.model);
    match http::post_json(cfg.gateway_addr, &path, body.as_bytes(), &headers_for(cfg, ing, &id.egress), cfg.probe_timeout) {
        Outcome::Response(r) if (200..300).contains(&r.status) => Served::Yes,
        Outcome::Response(r) => {
            // WHAT IT ACTUALLY SAID. Without this a declined cell is a bare verdict, and a whole
            // field answering 4xx for one rig-side reason reads as every gateway supporting nothing.
            let evidence = crate::cell::Evidence { status: r.status, body_snippet: crate::cell::Evidence::snippet(&String::from_utf8_lossy(r.body())) };
            // The verdict decides which of the two this is, and they are NOT interchangeable.
            // NotConfigured is the gateway's own answer about the pairing. NotVerified means the rig
            // could not get a fair reading, so nothing was learned about the gateway, and recording
            // it as "does not serve" would convict on the rig's failure.
            match persistent_transient_verdict(Observation { status: Some(r.status), mock_healthy }) {
                Verdict::NotConfigured => Served::No(Verdict::NotConfigured, evidence),
                Verdict::NotVerified => Served::Untestable(format!(
                    "status {} observed, but the rig could not confirm itself, so this says nothing about the gateway",
                    r.status
                )),
            }
        }
        // No HTTP answer at all: the gateway may never have been reached, so this says nothing
        // about it. Never a gateway fault.
        Outcome::ConnectionFailed(e) => Served::Untestable(format!("no connection to the gateway: {e}")),
        Outcome::TimedOut => Served::Untestable("the gateway accepted the connection and never answered".into()),
        Outcome::Malformed { message, .. } => Served::Untestable(format!("unparseable response: {message}")),
    }
}

/// Drives the load generator at one concurrency, for the searches.
struct SweepProbe<'a> {
    cfg: &'a RunConfig,
    path: String,
    body: String,
}

impl Probe for SweepProbe<'_> {
    fn probe(&mut self, concurrency: u32) -> Option<Sample> {
        // THE GENERATOR RUNS AS ITS OWN PINNED PROCESS, exactly as the Go one did.
        //
        // Running it in-process would put load generation on the orchestrator's cores, competing
        // with the gateway under test and with our own bookkeeping. The core split (gateway 0-3,
        // load 4-9, mock 10-15) IS the comparability basis of every published number: an unpinned
        // generator measures a different machine than a pinned one, and the difference is invisible
        // in the artifact. Same binary, separate process, same pinning the load generator has always
        // had.
        let stats = self.spawn_pinned(concurrency)?;
        // The OS refusing a thread means the window never ran at the requested concurrency: a RIG
        // limit, not a gateway result, so the search must stop rather than read a turnover.
        if stats.spawn_failed {
            eprintln!("loadgen: could not reach c={concurrency}; the rig refused a thread");
            return None;
        }
        // A window that produced nothing is UNMEASURED, not a zero.
        if stats.ok == 0 && stats.fail == 0 {
            return None;
        }
        Some(Sample { value: stats.rps() as f64, passed: stats.fail == 0 && stats.ok > 0 })
    }
}

impl SweepProbe<'_> {
    /// Run one window in a pinned child and read its stats line back.
    fn spawn_pinned(&self, concurrency: u32) -> Option<GenStats> {
        load_window(self.cfg, &self.path, &self.body, concurrency)
    }
}

/// Drive one pinned load window against the gateway and read the generator's stats line back.
///
/// Shared by the throughput search and the memory window so both put load on the box the same way:
/// same binary, same pinning, its own process. A memory number taken under a differently-generated
/// load is not comparable with a throughput number taken under this one.
/// Stop the gateway and start it again, returning only once it is ready to serve.
///
/// This exists for ONE reason: an idle memory reading has to come from a process that has not served
/// load, and after the throughput sweep no such process exists. Restarting is the only way to get one
/// back. The alternative that was in place - reading RSS where the process happened to be - published
/// post-load memory as idle and made every cell depend on the load the cell before it had run.
///
/// Errors carry the stage that failed, because "could not restart" and "restarted but never came
/// back" are different findings: the first leaves the gateway up, the second leaves it down and every
/// later cell in the grid will fail too.
pub fn restart_to_rest(spec: &crate::launch::LaunchSpec) -> Result<(), String> {
    crate::supervise::stop_and_wait(&spec.runtime, spec.port, Duration::from_secs(30))
        .map_err(|e| format!("stopping it failed: {e:?}"))?;
    let mut launcher = crate::launch::RealLauncher::default();
    crate::launch::launch_default(&mut launcher, spec)
        .map(|_| ())
        .map_err(|e| format!("it did not come back up: {e:?}"))
}

pub fn load_window(cfg: &RunConfig, path: &str, body: &str, concurrency: u32) -> Option<GenStats> {
    {
        let exe = std::env::current_exe().ok()?;
        let dur = cfg.sweep_duration_s.to_string();
        let conc = concurrency.to_string();
        let addr = cfg.gateway_addr.to_string();
        let mut cmd = match &cfg.load_cores {
            // taskset is how the rest of the harness pins, so the generator is pinned the same way.
            Some(cores) => {
                let mut c = std::process::Command::new("taskset");
                c.args(["-c", cores, exe.to_string_lossy().as_ref()]);
                c
            }
            None => std::process::Command::new(exe),
        };
        let out = cmd
            .args(["loadgen", &addr, path, &conc, &dur, body])
            .stderr(std::process::Stdio::inherit())
            .output()
            .ok()?;
        let line = String::from_utf8_lossy(&out.stdout);
        crate::loadgen::parse_ugen_line(line.trim()).into_value().map(|u| GenStats {
            ok: u.ok.max(0) as u64,
            fail: u.fail.max(0) as u64,
            elapsed_s: if u.rps > 0 { u.ok as f64 / u.rps as f64 } else { 0.0 },
            latencies_us: Vec::new(),
            spawn_failed: false,
        })
    }
}

pub struct CellPerf {
    pub max_proxy: Measurement<f64>,
    pub max_proxy_concurrency: Measurement<u32>,
    /// EVERY concurrency the search actually probed, in probe order. The peak is one point out of
    /// this; without it the published number cannot be re-derived or charted, and a reader has no
    /// way to see whether the search found a real turnover or simply ran out of range.
    pub points: Vec<crate::search::ProbedPoint>,
}

/// One load window at ONE concurrency. A point measurement, not a search.
///
/// This exists because asking a PEAK SEARCH for a maximum over a range of one is a category error:
/// the rig-ceiling reference and the box-qualification observation both want "what does this do at
/// exactly c", not a search with room to find a turnover on either side.
///
/// A point measurement makes no turnover claim, so there is nothing for a flanking check to refuse.
pub fn measure_at(cfg: &RunConfig, id: &CellId, concurrency: u32) -> Measurement<f64> {
    let Ok(ing) = id.ingress.parse::<Dialect>() else {
        return Measurement::absent_because(
            Absent::Untestable,
            format!("unknown ingress dialect {}", id.ingress),
        );
    };
    let mut p = SweepProbe { cfg, path: path_for(cfg, ing, &id.egress), body: ing.body(&cfg.model) };
    match p.probe(concurrency) {
        // The gate still applies: a window with failures is not a throughput reading, it is a window
        // the target could not serve cleanly.
        Some(s) if s.passed => Measurement::Measured(s.value),
        Some(_) => Measurement::absent_because(
            Absent::NotMeasured,
            format!("the window at c={concurrency} did not complete cleanly, so its rate is not a throughput reading"),
        ),
        None => Measurement::absent_because(
            Absent::NotMeasured,
            format!("no load window completed at c={concurrency}"),
        ),
    }
}

/// Find the gateway's throughput peak on one served cell.
pub fn sweep_cell(cfg: &RunConfig, id: &CellId, lo: u32, hi: u32) -> CellPerf {
    let Ok(ing) = id.ingress.parse::<Dialect>() else {
        return CellPerf {
            max_proxy: Measurement::absent(Absent::Untestable),
            max_proxy_concurrency: Measurement::absent(Absent::Untestable),
            // Nothing was probed, so there is no evidence to carry.
            points: Vec::new(),
        };
    };
    let mut p = SweepProbe { cfg, path: path_for(cfg, ing, &id.egress), body: ing.body(&cfg.model) };
    let start = ((lo + hi) / 2).max(lo);
    let r = search::peak_max(&mut p, lo, hi, start, 4);
    match r.peak.value() {
        Some(pt) => CellPerf {
            max_proxy: Measurement::Measured(pt.value),
            max_proxy_concurrency: Measurement::Measured(pt.concurrency),
            points: r.points.clone(),
        },
        None => CellPerf {
            // The search's own reason AND its evidence travel. Dropping the detail here was the one
            // place the "we discard the measurement" worry was actually true: the engine attaches
            // the lower bound as prose and the consumer boundary threw it away, leaving a bare null.
            max_proxy: match (r.peak.reason().cloned(), r.peak.detail()) {
                (Some(reason), Some(detail)) => Measurement::absent_because(reason, detail),
                (Some(reason), None) => Measurement::absent(reason),
                (None, _) => Measurement::absent(Absent::NotMeasured),
            },
            // Mirrors max_proxy's reason. Two different explanations for one absence, in the same
            // cell, is a smaller version of the reason-swapping this type exists to prevent.
            max_proxy_concurrency: Measurement::absent(
                r.peak.reason().cloned().unwrap_or(Absent::NotMeasured),
            ),
            // A search that found no publishable peak still probed real rungs, and those rungs are
            // exactly what explains why it found nothing. Dropping them here would leave a null with
            // no evidence beside it.
            points: r.points.clone(),
        },
    }
}

/// Is the mock answering? Every not-served verdict is conditioned on this, because a rig that went
/// away underneath a probe cannot be used to attribute anything to the gateway.
pub fn mock_healthy(cfg: &RunConfig) -> bool {
    let d = Dialect::Openai;
    matches!(
        http::post_json(
            cfg.mock_addr,
            &d.mock_direct_path(&cfg.model),
            d.body(&cfg.model).as_bytes(),
            // The mock is spoken to in the dialect being checked, with no gateway routing headers:
            // those select an upstream INSIDE a gateway and mean nothing to the mock itself.
            &d.auth_headers(&cfg.auth),
            Duration::from_secs(5),
        ),
        Outcome::Response(r) if (200..300).contains(&r.status)
    )
}

pub struct CellResult {
    pub outcome: CellOutcome,
    /// Every metric the engine took on this cell, keyed by the artifact field it fills. `None` for a
    /// cell that was not served: there is nothing to measure, and an empty map would read as
    /// "measured nothing" rather than "never asked".
    pub metrics: Option<std::collections::BTreeMap<&'static str, Measurement<f64>>>,
    /// The evidence behind those scalars: the rungs the throughput search probed and the resident
    /// memory readings taken across the load window. `None` alongside `metrics` for a cell that was
    /// never measured, and empty for one that was measured but produced no series.
    pub series: Option<crate::metric::Series>,
}

/// Walk the grid: probe every pairing, sweep the ones that are served.
pub fn run_grid(cfg: &RunConfig, lo: u32, hi: u32) -> Vec<CellResult> {
    run_grid_with(cfg, lo, hi, metric::METRICS)
}

/// The same walk, over an explicit metric list, so a test can drive the grid without performing
/// every real measurement.
pub fn run_grid_with(cfg: &RunConfig, lo: u32, hi: u32, metrics: &[&dyn metric::Metric]) -> Vec<CellResult> {
    let healthy = mock_healthy(cfg);
    let mut out = Vec::new();
    for eg in &cfg.dialects {
        for ing in &cfg.dialects {
            let id = CellId::new(ing.as_str(), eg.as_str());
            let served = probe_cell(cfg, &id, healthy);
            // THE ENGINE, IN TWO LINES: if the cell is served, run every metric on it. The list of
            // metrics lives in one place (`metric::METRICS`) rather than being reached for here, so
            // a measurement cannot be implemented, tested, and then silently never taken.
            let (metrics, series) = if served.is_measurable() {
                let ctx = metric::CellCtx { cfg, id: &id, dialect: *ing, min_conc: lo, max_conc: hi };
                let (m, s) = metric::process_cell_with(&ctx, metrics);
                (Some(m), Some(s))
            } else {
                (None, None)
            };
            let outcome = match served {
                Served::Yes => CellOutcome::served(id),
                Served::No(v, ev) => {
                    let n = format!("probed and answered {} (HTTP {})", v.token(), ev.status);
                    CellOutcome::not_served(id, v, ev, n)
                }
                Served::Untestable(r) => CellOutcome::untestable(id, r),
            };
            out.push(CellResult { outcome, metrics, series });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn cfg_for(gw: SocketAddr, mock: SocketAddr) -> RunConfig {
        RunConfig {
            gateway_addr: gw,
            mock_addr: mock,
            model: "m".into(),
            auth: "dummy".into(),
            dialects: vec![Dialect::Openai],
            sweep_duration_s: 1,
            probe_timeout: Duration::from_secs(2),
            load_cores: None,
            static_headers: Vec::new(),
            egress_headers: Default::default(),
            runtime: crate::manifest::Runtime::Native { proc_match: "test-fixture".into() },
            declared_path: String::new(),
            cell_paths: Default::default(),
            relaunch: None,
        }
    }

    /// A server that answers every request with a fixed status.
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

    #[test]
    fn a_2xx_is_served() {
        let gw = serve(200);
        let cfg = cfg_for(gw, gw);
        assert_eq!(probe_cell(&cfg, &CellId::new("openai", "openai"), true), Served::Yes);
    }

    // A real error status with a healthy rig is the GATEWAY's own answer about this pairing.
    #[test]
    fn a_real_error_status_with_a_healthy_rig_is_the_gateways_answer() {
        let gw = serve(404);
        let cfg = cfg_for(gw, gw);
        let s = probe_cell(&cfg, &CellId::new("openai", "openai"), true);
        assert!(matches!(s, Served::No(..)), "got {s:?}");
    }

    // The SAME status with an unhealthy rig says nothing about the gateway, so it must not be
    // recorded as the gateway refusing. This is the rig/gateway distinction the board rests on.
    #[test]
    fn the_same_status_with_an_unhealthy_rig_is_not_blamed_on_the_gateway() {
        let gw = serve(404);
        let cfg = cfg_for(gw, gw);
        let s = probe_cell(&cfg, &CellId::new("openai", "openai"), false);
        assert!(!matches!(s, Served::No(..)), "an unconfirmed rig cannot convict the gateway: {s:?}");
    }

    // Nothing listening is never the gateway's fault: it may never have been reached.
    #[test]
    fn an_unreachable_gateway_is_untestable_not_unserved() {
        let dead: SocketAddr = "127.0.0.1:1".parse().expect("literal");
        let cfg = cfg_for(dead, dead);
        let s = probe_cell(&cfg, &CellId::new("openai", "openai"), true);
        assert!(matches!(s, Served::Untestable(_)), "got {s:?}");
    }

    #[test]
    fn an_unknown_dialect_is_untestable_rather_than_a_default_path() {
        let gw = serve(200);
        let cfg = cfg_for(gw, gw);
        let s = probe_cell(&cfg, &CellId::new("nonsense", "openai"), true);
        assert!(matches!(s, Served::Untestable(_)));
    }

    #[test]
    fn mock_health_is_measured_not_assumed() {
        let up = serve(200);
        assert!(mock_healthy(&cfg_for(up, up)));
        let dead: SocketAddr = "127.0.0.1:1".parse().expect("literal");
        assert!(!mock_healthy(&cfg_for(dead, dead)));
    }

    // Every pairing appears, served or not. A dropped row hides a failure.
    #[test]
    fn the_grid_records_every_pairing() {
        let gw = serve(200);
        let mut cfg = cfg_for(gw, gw);
        cfg.dialects = vec![Dialect::Openai, Dialect::Anthropic];
        // An explicit, empty metric list: this test is about the SHAPE of the grid, so it must not
        // pay for every real measurement to assert that every pairing appears.
        let rows = run_grid_with(&cfg, 1, 2, &[]);
        assert_eq!(rows.len(), 4);
    }

    // An unserved cell carries NO metrics. A number attached to a pairing the gateway does not serve
    // is a number about nothing.
    #[test]
    fn an_unserved_cell_carries_no_metrics() {
        let gw = serve(404);
        let cfg = cfg_for(gw, gw);
        let rows = run_grid_with(&cfg, 1, 2, &[]);
        for r in &rows {
            assert!(r.metrics.is_none(), "{} must not carry metrics", r.outcome.id);
        }
    }
    // WHICH URL A CELL IS DRIVEN AT, in precedence order.
    //
    // Two real gateways mount their compatible API somewhere other than the dialect's standard path,
    // and the probe ignored the manifest and used the standard one. Both answered a truthful 404 on
    // every cell and the artifact published them as serving nothing at all: a false claim about
    // somebody's product, produced entirely by us.
    //
    // A per-cell entry exists for the gateways that route a same-dialect request differently from a
    // translating one. It is keyed by the full cell, so choosing it is a deliberate, visible act in
    // that gateway's data rather than something the engine infers.
    #[test]
    fn a_cell_is_driven_at_its_own_path_then_the_declared_one_then_the_standard() {
        let mut cfg = cfg_for("127.0.0.1:1".parse().unwrap(), "127.0.0.1:2".parse().unwrap());

        // Nothing declared: the dialect's standard path.
        assert_eq!(path_for(&cfg, Dialect::Openai, "openai"), "/v1/chat/completions");

        // A declared path that is a longer form of the standard one applies to that dialect, and to
        // that dialect only: a gateway mounting its OpenAI API under a prefix has not moved anyone
        // else's API.
        cfg.declared_path = "/openai/v1/chat/completions".to_string();
        assert_eq!(path_for(&cfg, Dialect::Openai, "anthropic"), "/openai/v1/chat/completions");
        assert_eq!(path_for(&cfg, Dialect::Anthropic, "anthropic"), "/v1/messages");

        // A cell's own path wins over both, and ONLY for that cell. The neighbouring cell in the
        // same row keeps the declared path, which is what stops one entrant being measured on a
        // provider-pinned route while the rest of its row is measured on the unified one.
        cfg.cell_paths.insert("openai>openai".to_string(), "/passthrough".to_string());
        assert_eq!(path_for(&cfg, Dialect::Openai, "openai"), "/passthrough");
        assert_eq!(path_for(&cfg, Dialect::Openai, "anthropic"), "/openai/v1/chat/completions");
    }

}
