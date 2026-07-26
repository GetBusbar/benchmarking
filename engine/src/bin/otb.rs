// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The engine binary. Subcommands mirror the shell functions one for one, taking the same input on
// stdin and printing the same thing on stdout, so a differential harness can run both
// implementations over generated inputs and diff them. Parity that is executed beats parity that is
// reviewed: a side-by-side reading of these two searches missed four defects that a diff would have
// caught on the first random case.

use std::io::Read;
use std::process::ExitCode;

use otb_engine::gen::{self, GenConfig};
use otb_engine::stats::{self, Sample, Verdict};
use std::time::Duration;

fn usage() -> ExitCode {
    eprintln!(
        "otb {}\n\nSamples arrive on stdin as \"<t_s> <mib>\" per line, matching the shell's series files.\n\n\
         \x20 plateau-check <trend_pct> <range_pct>   prints 1 for steady, 0 otherwise (mirrors plateau_check)\n\
         \x20 growth-rate                             prints MiB/min to 3dp, or nothing (mirrors plateau_growth_rate)\n\
         \x20 window <window_s>                       prints the trailing window (mirrors plateau_window)\n\
         \x20 version\n",
        env!("CARGO_PKG_VERSION")
    );
    ExitCode::from(2)
}

fn read_samples() -> Vec<Sample> {
    let mut buf = String::new();
    if std::io::stdin().read_to_string(&mut buf).is_err() {
        return Vec::new();
    }
    buf.lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let t = it.next()?.parse::<f64>().ok()?;
            let v = it.next()?.parse::<f64>().ok()?;
            Some(Sample { t_s: t, mib: v })
        })
        .collect()
}

/// UTC stamp in the shape the existing snapshot corpus already uses.
fn utc_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (h, m, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // Civil-from-days, so the stamp is a real date rather than an epoch count.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if mth <= 2 { y + 1 } else { y };
    format!("{year:04}-{mth:02}-{d:02}T{h:02}-{m:02}-{sec:02}Z")
}

fn arg_f64(args: &[String], i: usize, default: f64) -> f64 {
    args.get(i).and_then(|s| s.parse::<f64>().ok()).unwrap_or(default)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        // The load generator, as a subcommand of the same binary. One artifact to cross-compile and
        // ship, one version stamp, and the stats line is a shared struct rather than a text format
        // parsed by hand on both sides of a process boundary.
        Some("loadgen") => {
            let addr = match args.get(1).and_then(|a| a.parse().ok()) {
                Some(a) => a,
                None => {
                    eprintln!("usage: otb loadgen <ip:port> <path> <concurrency> <duration_s> [body]");
                    return ExitCode::from(2);
                }
            };
            let path = args.get(2).cloned().unwrap_or_else(|| "/".into());
            let conc: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
            let dur: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(5);
            let body = args.get(5).cloned().unwrap_or_else(|| "{}".into());
            let stats = gen::run(&GenConfig {
                addr,
                path,
                body,
                headers: vec![("authorization".into(), "Bearer dummy".into())],
                concurrency: conc,
                duration: Duration::from_secs(dur),
                ttft_ms: 0,
            });
            println!("{}", stats.stats_line());
            ExitCode::SUCCESS
        }
        // End to end: probe the grid on a live gateway and sweep what it serves.
        Some("smoke") => {
            use otb_engine::ingress::Dialect;
            use otb_engine::run::{run_grid, RunConfig};
            let gw = args.get(1).and_then(|a| a.parse().ok());
            let mk = args.get(2).and_then(|a| a.parse().ok());
            let (Some(gateway_addr), Some(mock_addr)) = (gw, mk) else {
                eprintln!("usage: otb smoke <gateway ip:port> <mock ip:port> [model]");
                return ExitCode::from(2);
            };
            let cfg = RunConfig {
                gateway_addr,
                mock_addr,
                model: args.get(3).cloned().unwrap_or_else(|| "gpt-4o-mini".into()),
                auth: "dummy".into(),
                dialects: vec![Dialect::Openai, Dialect::Anthropic, Dialect::Gemini],
                sweep_duration_s: 2,
                probe_timeout: Duration::from_secs(10),
                load_cores: std::env::var("LOADCORES").ok(),
            };
            println!("mock healthy: {}", otb_engine::run::mock_healthy(&cfg));
            for r in run_grid(&cfg, 4, 64) {
                let perf = match &r.perf {
                    Some(p) => match (p.max_proxy.copied(), p.max_proxy_concurrency.copied()) {
                        (Some(v), Some(c)) => format!("max_proxy={v:.0} rps @ c={c}"),
                        _ => format!("max_proxy=n/a ({:?})", p.max_proxy.reason()),
                    },
                    None => "not measured".into(),
                };
                println!("{:<28} {:<12} {}", r.outcome.id.to_string(), format!("{:?}", r.outcome.served).chars().take(11).collect::<String>(), perf);
            }
            ExitCode::SUCCESS
        }
        // The whole suite for one gateway: probe the grid, sweep what is served, judge each peak
        // against the rig at the same operating point, and write the snapshot.
        Some("run") => {
            use otb_engine::ingress::Dialect;
            use otb_engine::manifest::Manifest;
            use otb_engine::suite::{run_suite, SuiteConfig};
            let Some(manifest_path) = args.get(1) else {
                eprintln!("usage: otb run <manifest.json> <gateway ip:port> <mock ip:port> [results_dir]");
                return ExitCode::from(2);
            };
            let text = match std::fs::read_to_string(manifest_path) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("cannot read manifest {manifest_path}: {e}");
                    return ExitCode::FAILURE;
                }
            };
            let manifest: Manifest = match serde_json::from_str(&text) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("manifest {manifest_path} is not valid: {e}");
                    return ExitCode::FAILURE;
                }
            };
            if let Err(e) = manifest.validate() {
                eprintln!("manifest {manifest_path} is incomplete: {e}");
                return ExitCode::FAILURE;
            }
            let (Some(gw), Some(mk)) = (
                args.get(2).and_then(|a| a.parse().ok()),
                args.get(3).and_then(|a| a.parse().ok()),
            ) else {
                eprintln!("usage: otb run <manifest.json> <gateway ip:port> <mock ip:port> [results_dir]");
                return ExitCode::from(2);
            };
            let results_dir = args.get(4).cloned().unwrap_or_else(|| "results/snapshots".into());
            if let Err(e) = std::fs::create_dir_all(&results_dir) {
                eprintln!("cannot create {results_dir}: {e}");
                return ExitCode::FAILURE;
            }
            // The grid and the search range are overridable because the full default run is 36 cells
            // x a peak search x a pinned child per rung, and there was no way to ask for a smaller
            // one. That is not only a convenience: an end-to-end run that cannot be shrunk cannot be
            // tested, and this entry point had no test at all. A field run passes nothing here and
            // gets the same defaults as before.
            let dialects = match std::env::var("OTB_DIALECTS") {
                Ok(list) => {
                    let parsed: Vec<Dialect> = list.split(',').filter_map(|d| d.trim().parse().ok()).collect();
                    if parsed.is_empty() {
                        eprintln!("OTB_DIALECTS={list:?} named no dialect this build knows");
                        return ExitCode::from(2);
                    }
                    parsed
                }
                Err(_) => Dialect::ALL.to_vec(),
            };
            let env_u32 = |k: &str, d: u32| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
            let cfg = SuiteConfig {
                manifest,
                mock_addr: mk,
                results_dir: results_dir.into(),
                dialects,
                sweep_duration_s: arg_f64(&args, 5, 6.0) as u64,
                load_cores: std::env::var("LOADCORES").ok(),
                min_conc: env_u32("OTB_MIN_CONC", 4),
                max_conc: env_u32("OTB_MAX_CONC", 512),
                measured_at: utc_stamp(),
                arch: std::env::var("BENCH_ARCH").unwrap_or_else(|_| "unknown".into()),
            };
            match run_suite(&cfg, gw) {
                Ok(paths) => {
                    println!("wrote {}", paths.current.display());
                    println!("wrote {}", paths.historical.display());
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("snapshot not written: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("version") => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        // The shell is handed an ALREADY-WINDOWED file, so window over everything here to match.
        Some("plateau-check") => {
            let samples = read_samples();
            let steady = matches!(
                stats::plateau_check(&samples, f64::INFINITY, arg_f64(&args, 1, 1.0), arg_f64(&args, 2, 2.0)),
                Verdict::Steady
            );
            println!("{}", u8::from(steady));
            ExitCode::SUCCESS
        }
        // %.3f matches plateau_growth_rate's own printf, so a diff compares numbers rather than
        // float formatting. An unmeasurable rate prints nothing, exactly as the shell's awk exits.
        Some("growth-rate") => {
            if let Some(r) = stats::growth_rate(&read_samples()).copied() {
                println!("{r:.3}");
            }
            ExitCode::SUCCESS
        }
        Some("window") => {
            for s in stats::window(&read_samples(), arg_f64(&args, 1, 60.0)) {
                println!("{} {}", s.t_s, s.mib);
            }
            ExitCode::SUCCESS
        }
        _ => usage(),
    }
}
