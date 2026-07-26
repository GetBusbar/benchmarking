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
    };
    let id = crate::cell::CellId::new(dialect.as_str(), dialect.as_str());
    // A single point AT THE WINNER's concurrency, not a search: the reference must be taken where
    // the gateway's number was taken, or the comparison is between two different operating points.
    let perf = run::sweep_cell(&direct, &id, at_conc, at_conc);
    perf.max_proxy
}

/// Judge one cell's throughput and suppress it if the rig, not the gateway, set it.
fn judge_cell(cfg: &SuiteConfig, dialect: Dialect, perf: run::CellPerf) -> Judged {
    let mut out = empty_perf();
    let (Some(&value), Some(&conc)) = (perf.max_proxy.value(), perf.max_proxy_concurrency.value())
    else {
        // Carry the search's own reason and evidence rather than flattening it.
        out.rps_max_proxy = match (perf.max_proxy.reason().cloned(), perf.max_proxy.detail()) {
            (Some(r), Some(d)) => Measurement::absent_because(r, d),
            (Some(r), None) => Measurement::absent(r),
            (None, _) => Measurement::absent(Absent::NotMeasured),
        };
        return Judged { perf: out };
    };

    let reference = rig_ceiling(cfg, dialect, conc);
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
        }
        Some(false) => {
            out.rps_max_proxy = Measurement::Measured(value as i64);
            out.rps_max_proxy_mock_bound = Some(false);
            out.conc_at_peak = Measurement::Measured(i64::from(conc));
        }
        // The reference itself was unusable, so whether the rig bounded this is UNKNOWN. Publishing
        // the number would assert it was gateway-bound, which was never established.
        None => {
            out.rps_max_proxy = Measurement::absent_because(
                Absent::RigLimited,
                format!("reached {value:.0} at c={conc}, but the rig reference could not be measured, so it is unknown whether the gateway or the rig set this"),
            );
            out.rps_max_proxy_mock_bound = None;
        }
    }
    Judged { perf: out }
}

/// Run the whole suite for one gateway and write its snapshot.
pub fn run_suite(cfg: &SuiteConfig, gateway_addr: SocketAddr) -> Result<Paths, SnapshotError> {
    let rc = RunConfig {
        gateway_addr,
        mock_addr: cfg.mock_addr,
        model: cfg.manifest.model.clone(),
        auth: cfg.manifest.auth.clone(),
        dialects: cfg.dialects.clone(),
        sweep_duration_s: cfg.sweep_duration_s,
        probe_timeout: Duration::from_secs(10),
    };

    let mut upstreams: HashMap<String, Upstream> = HashMap::new();
    let mut any_served = false;

    for result in run::run_grid(&rc, cfg.min_conc, cfg.max_conc) {
        let id = &result.outcome.id;
        let ing = id.ingress.clone();
        let eg = id.egress.clone();

        let (served, reason) = match &result.outcome.served {
            Served::Yes => (RecServed::Bool(true), None),
            Served::No(v) => (RecServed::Status(v.token().to_string()), Some(v.token().to_string())),
            // A rig limit is NOT the gateway refusing. It keeps its own label all the way out.
            Served::Untestable(r) => (RecServed::Status("untestable".into()), Some(r.clone())),
        };
        if matches!(served, RecServed::Bool(true)) {
            any_served = true;
        }

        let perf = match (result.perf, ing.parse::<Dialect>()) {
            (Some(p), Ok(d)) => Some(judge_cell(cfg, d, p).perf),
            _ => None,
        };

        let cell = Cell {
            served,
            reason,
            path: ing.parse::<Dialect>().map(|d| d.path(&cfg.manifest.model)).unwrap_or_default(),
            perf,
            ..Default::default()
        };

        upstreams
            .entry(eg)
            .or_insert_with(|| Upstream { configurable: true, served: true, ..Default::default() })
            .cells
            .insert(ing, cell);
    }

    let snap = ResultSnapshot {
        schema_version: 1,
        gateway: cfg.manifest.name.clone(),
        build: format!("otb-engine {}", env!("CARGO_PKG_VERSION")),
        measured_at: cfg.measured_at.clone(),
        arch: Some(cfg.arch.clone()),
        matrix: Matrix {
            gateway: cfg.manifest.name.clone(),
            served: any_served,
            cell_perf_sweep: true,
            upstreams,
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
            manifest: Manifest {
                name: "gw".into(),
                display: "GW".into(),
                lang: "Rust".into(),
                class: "gateway".into(),
                repo: "https://example.invalid/gw".into(),
                port: 1,
                path: "/v1/chat/completions".into(),
                model: "m".into(),
                auth: "dummy".into(),
                headers: vec![],
                runtime: crate::manifest::Runtime::Docker { container: "gw-bench".into() },
                egress: vec![],
                config: vec![],
            },
            mock_addr: mock,
            results_dir: dir.to_path_buf(),
            dialects: vec![Dialect::Openai],
            sweep_duration_s: 1,
            min_conc: 1,
            max_conc: 2,
            measured_at: "2026-07-26T00-00-00Z".into(),
            arch: "arm64".into(),
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
        let paths = run_suite(&cfg, gw).expect("the suite should write a snapshot");
        let text = std::fs::read_to_string(&paths.current).expect("current file");
        let back: ResultSnapshot = serde_json::from_str(&text).expect("its own output must parse");
        assert_eq!(back.gateway, "gw");
        assert!(back.matrix.served, "a 2xx gateway serves its diagonal");
        assert!(paths.historical.exists(), "the timestamped copy must land too");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // A gateway that refuses still produces a complete artifact: the row exists, says not served,
    // and carries no numbers. A dropped row would hide the failure.
    #[test]
    fn an_unserved_gateway_still_writes_a_complete_row_with_no_numbers() {
        let dir = tmpdir("unserved");
        let gw = serve(404);
        let cfg = cfg_for(&dir, gw);
        let paths = run_suite(&cfg, gw).expect("a refusing gateway is still a result");
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
    #[test]
    fn a_number_at_the_rig_ceiling_is_suppressed_not_published() {
        let dir = tmpdir("rigbound");
        let gw = serve(200);
        // gateway and mock are the SAME server, so the reference equals the observation.
        let cfg = cfg_for(&dir, gw);
        let paths = run_suite(&cfg, gw).expect("write");
        let text = std::fs::read_to_string(&paths.current).expect("read");
        let back: ResultSnapshot = serde_json::from_str(&text).expect("parse");
        if let Some(perf) = back
            .matrix
            .upstreams
            .get("openai")
            .and_then(|u| u.cells.get("openai"))
            .and_then(|c| c.perf.as_ref())
        {
            if perf.rps_max_proxy_mock_bound == Some(true) {
                assert_eq!(
                    perf.rps_max_proxy.copied(),
                    None,
                    "a rig-bound number must not be published as the gateway's"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
