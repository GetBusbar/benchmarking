// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The engine binary. Subcommands mirror the shell functions one for one, same stdin input and
// stdout output, so a differential harness can run both implementations and diff them.
//
// Separate crate target from the library (its own root, not lib.rs), so it needs its own copy of
// the test-only unwrap/expect/panic exemption lib.rs documents.
#![cfg_attr(test, allow(clippy::panic, clippy::unwrap_used, clippy::expect_used))]

use std::io::Read;
use std::process::ExitCode;

use otb_engine::gen::{self, GenConfig};
use otb_engine::stats::{self, Sample};
use std::time::Duration;

/// If `commands` left a minted credential at `<gw_dir>/.minted-auth`, its trimmed contents are what
/// every probe should authenticate with instead of the manifest's declared (placeholder) `auth`.
/// `None` for absent/unreadable/whitespace-only, all meaning "nothing was minted".
fn resolve_minted_auth(gw_dir: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(gw_dir.join(".minted-auth")).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
}

/// A gateway that mints its own credential can't be measured through a restart: `resolve_minted_auth`
/// runs once after the initial launch, but the memory phase later stops and relaunches the gateway
/// (`run::restart_to_rest`), replaying `commands` and minting a fresh credential every later request
/// won't have. The engine can't re-resolve it from here (the restart happens deep inside the grid,
/// against a credential the run config captured by value), so it refuses the combination instead of
/// publishing a run whose second half authenticated with a dead token. Returns the refusal to print,
/// or `None` when there is nothing to refuse.
fn stale_minted_auth_refusal(minted: Option<&str>, harness_restarts_it: bool) -> Option<String> {
    if minted.is_none() || !harness_restarts_it {
        return None;
    }
    Some(
        "this gateway minted its own credential at boot, and the harness owns its lifetime: the \
         memory phase restarts it mid-run and replays `commands`, which would mint a DIFFERENT \
         credential while every later request kept using this one. Refusing to measure rather than \
         publish a run whose second half authenticated with a stale token. Fix by re-resolving \
         .minted-auth after each restart (run.rs `restart_to_rest`), or by giving the gateway a \
         credential it accepts across boots."
            .to_string(),
    )
}

/// The loud end-of-run stop failure, or `None` if the gateway really is gone.
///
/// A gateway that outlives the stop budget keeps the port and cores the NEXT gateway needs, so a
/// discarded failure here surfaces one gateway later as that one "never becoming ready".
fn end_of_run_stop_failure(
    identity: &str,
    stop: &Result<(), otb_engine::supervise::SuperviseError>,
) -> Option<String> {
    let err = stop.as_ref().err()?;
    Some(format!(
        "{identity} survived the stop budget ({err}); it still holds its port and cores, and the \
         NEXT gateway launched on this box will fail to boot because of it. Kill it before running \
         anything else here."
    ))
}

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

/// Real ISO-8601, colons and all: `snapshot.rs`'s `write_snapshot` derives the filesystem-safe
/// historical filename by replacing ':' with '-' on exactly this shape, and the site generator's
/// `Date.parse` returns NaN without the colons, rendering `measured_at` as null.
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
    format!("{year:04}-{mth:02}-{d:02}T{h:02}:{m:02}:{sec:02}Z")
}

/// The id every container this invocation creates is scoped to. `OTB_RUN_ID` when the orchestrator
/// set one (run-on-ec2.sh's own `RUN_ID`), otherwise this process's pid, enough for two runs
/// sharing a box to avoid naming each other's containers.
fn run_scope() -> String {
    std::env::var("OTB_RUN_ID")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| std::process::id().to_string())
}

fn arg_f64(args: &[String], i: usize, default: f64) -> f64 {
    args.get(i)
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(default)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        // The load generator, as a subcommand of the same binary: one artifact to ship, one version
        // stamp, and the stats line is a shared struct rather than a hand-parsed text format.
        Some("loadgen") => {
            let addr = match args.get(1).and_then(|a| a.parse().ok()) {
                Some(a) => a,
                None => {
                    eprintln!(
                        "usage: otb loadgen <ip:port> <path> <concurrency> <duration_s> [body]"
                    );
                    return ExitCode::from(2);
                }
            };
            let path = args.get(2).cloned().unwrap_or_else(|| "/".into());
            let conc: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(1);
            let dur: u64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(5);
            let body = args.get(5).cloned().unwrap_or_else(|| "{}".into());
            // Headers come from the spawning engine via the environment, not a hardcoded placeholder:
            // a fixed `authorization: Bearer dummy` here would silently mismatch most dialects' real
            // credentials, failing every load window and reading as the search finding no passing
            // concurrency rather than as our own credential fault.
            //
            // Unset means NO headers, never a placeholder (see `loadgen::decode_headers`): a wrong
            // credential is worse than none. The warning below is so a hand-run `otb loadgen` against
            // an authenticating gateway doesn't read as the gateway falling over.
            let headers = otb_engine::loadgen::decode_headers(
                std::env::var(otb_engine::loadgen::HEADERS_ENV)
                    .ok()
                    .as_deref(),
            );
            if headers.is_empty() {
                eprintln!(
                    "loadgen: {} is unset or empty, so this window sends no credential; every request \
                     will fail against a gateway that requires one",
                    otb_engine::loadgen::HEADERS_ENV
                );
            }
            let stats = gen::run(&GenConfig {
                addr,
                path,
                body,
                headers,
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
                // No manifest, so there's nothing to vary the egress column by.
                egress_models: Default::default(),
                model: args.get(3).cloned().unwrap_or_else(|| "gpt-4o-mini".into()),
                auth: "dummy".into(),
                dialects: vec![Dialect::Openai, Dialect::Anthropic, Dialect::Gemini],
                sweep_duration_s: 2,
                probe_timeout: Duration::from_secs(10),
                load_cores: std::env::var("LOADCORES").ok(),
                // `smoke` drives an already-running gateway it didn't pin, so no core list it can
                // honestly claim; empty means utilisation reports absent rather than measuring some
                // other process's cores as this gateway's.
                gw_cores: String::new(),
                // No manifest, so no declared identity to measure memory against. An empty match
                // resolves to nothing (enforced in `supervise::select_matches`) rather than
                // `pgrep -f ""` matching every process on the box.
                static_headers: Vec::new(),
                egress_headers: Default::default(),
                runtime: otb_engine::manifest::Runtime::Native {
                    proc_match: String::new(),
                },
                // Someone else started this target, so the harness must never restart it, and it
                // takes no manifest, so no declared path either.
                declared_path: String::new(),
                cell_paths: Default::default(),
                // No manifest, so no declared capability grid: every cell is probed.
                matrix: Vec::new(),
                matrix_note: String::new(),
                untestable_cells: Vec::new(),
                untestable_note: String::new(),
                relaunch: None,
                relaunch_commands: Vec::new(),
                relaunch_launcher: Default::default(),
            };
            println!("mock healthy: {}", otb_engine::run::mock_healthy(&cfg));
            for r in run_grid(&cfg, 4, 64) {
                // Print every metric the engine took, not a curated one, so a group that silently
                // returned nothing isn't hidden.
                let perf = match &r.metrics {
                    Some(m) => m
                        .iter()
                        .map(|(name, value)| match value.copied() {
                            Some(v) => format!("{name}={v:.0}"),
                            None => format!("{name}=n/a({:?})", value.reason()),
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                    None => "not measured".into(),
                };
                println!(
                    "{:<28} {:<12} {}",
                    r.outcome.id.to_string(),
                    format!("{:?}", r.outcome.served)
                        .chars()
                        .take(11)
                        .collect::<String>(),
                    perf
                );
            }
            ExitCode::SUCCESS
        }
        // The whole suite for one gateway: probe the grid, sweep what's served, judge each peak
        // against the rig at the same operating point, write the snapshot.
        Some("run") => {
            use otb_engine::manifest::Manifest;
            use otb_engine::suite::{run_suite, SuiteConfig};
            let Some(manifest_path) = args.get(1) else {
                eprintln!(
                    "usage: otb run <manifest.json> <gateway ip:port> <mock ip:port> [results_dir]"
                );
                return ExitCode::from(2);
            };
            // A gateway is a directory (definition.json plus sidecars), not a file, but passing the
            // definition file itself still resolves to the same directory.
            let dir = {
                let p = std::path::Path::new(manifest_path);
                if p.is_dir() {
                    p.to_path_buf()
                } else {
                    p.parent().unwrap_or(p).to_path_buf()
                }
            };
            let mut manifest: Manifest = match Manifest::load(&dir) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
            };
            // Bind every container this invocation creates to this run: without it, the container
            // name is the manifest's alone, so a second run's boot-retry `docker rm -f` would delete
            // the first run's container mid-measurement.
            manifest.runtime = manifest.runtime.scoped_to_run(&run_scope());
            if let Err(e) = manifest.validate() {
                eprintln!("manifest {manifest_path} is incomplete: {e}");
                return ExitCode::FAILURE;
            }
            // Config-necessity gate, checked here where refusing still costs nothing: every gateway
            // config must be the bare minimum required to run, or it hasn't earned a published number.
            // Defaults are empty for now (no per-gateway "ships with" source yet), so only the
            // structural lint rules (unusable key, duplicate claim) can fire here.
            let findings =
                otb_engine::config_lint::lint(&manifest, &otb_engine::config_lint::Defaults::new());
            for f in &findings {
                eprintln!("config lint: {}", f.message);
            }
            if otb_engine::config_lint::blocks(&findings) {
                eprintln!("manifest {manifest_path} does not meet the config-necessity standard; refusing to measure it");
                return ExitCode::FAILURE;
            }
            // The gateway's port is declared in its definition, not repeated by the caller: a
            // caller-supplied port is a second spelling of one fact and risks measuring whatever
            // answered on it. An explicit address is still accepted for driving something already
            // running (via OTB_GATEWAY_ADDR below).
            let Some(mk) = args
                .get(2)
                .and_then(|a| a.parse::<std::net::SocketAddr>().ok())
            else {
                eprintln!("usage: otb run <gateway dir> <mock ip:port> [results_dir] [sweep_s]");
                eprintln!("  the gateway's own address comes from its definition; OTB_GATEWAY_ADDR overrides it");
                return ExitCode::from(2);
            };
            let gw: std::net::SocketAddr = match std::env::var("OTB_GATEWAY_ADDR") {
                Ok(a) => match a.parse() {
                    Ok(a) => a,
                    Err(e) => {
                        eprintln!("OTB_GATEWAY_ADDR={a:?} is not an address: {e}");
                        return ExitCode::FAILURE;
                    }
                },
                Err(_) => match format!("127.0.0.1:{}", manifest.port).parse() {
                    Ok(a) => a,
                    Err(e) => {
                        eprintln!(
                            "{} declares port {} which is not usable: {e}",
                            manifest.name, manifest.port
                        );
                        return ExitCode::FAILURE;
                    }
                },
            };
            let results_dir = args
                .get(3)
                .cloned()
                .unwrap_or_else(|| "results/snapshots".into());
            if let Err(e) = std::fs::create_dir_all(&results_dir) {
                eprintln!("cannot create {results_dir}: {e}");
                return ExitCode::FAILURE;
            }
            // Grid and search range are overridable: the full default run is 36 cells x a peak
            // search x a pinned child per rung, and an end-to-end run that can't be shrunk can't be
            // tested. Passing nothing gets the same defaults a field run uses.
            let raw_dialects = std::env::var("OTB_DIALECTS").ok();
            let dialects = match otb_engine::ingress::dialects_from(raw_dialects.as_deref()) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::from(2);
                }
            };
            let env_u32 = |k: &str, d: u32| {
                std::env::var(k)
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(d)
            };
            let gw_dir = dir.clone();
            let gw_cores = std::env::var("OTB_GW_CORES").unwrap_or_else(|_| "0-3".into());

            // Render the config before launching: most containers mount a file the harness writes,
            // and launching before it exists produces a gateway that starts and dies, reading as
            // the gateway being broken.
            match manifest.render_configs(&gw_cores, mk.port(), &gw_dir) {
                Ok(written) => {
                    for (path, _) in &written {
                        println!("rendered {}", path.display());
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
            }

            let mut cfg = SuiteConfig {
                manifest,
                gw_dir: gw_dir.clone(),
                gw_cores: gw_cores.clone(),
                mock_addr: mk,
                results_dir: results_dir.into(),
                dialects,
                sweep_duration_s: arg_f64(&args, 4, 6.0) as u64,
                load_cores: std::env::var("LOADCORES").ok(),
                // Engine's own default, not an orchestrator setting: an unset override should get
                // the wide range, not a narrow one silently clipping a fast gateway's ceiling. 512
                // was too narrow to find some entrants' true peak; 65536 is wrong the other way,
                // since `gen.rs::run` spawns one real OS thread per unit of concurrency
                // (`std::thread::Builder::spawn_scoped`) pinned to LOADCORES' handful of cores, and
                // ramping toward 65536 threads there measures the rig's own scheduler thrashing, not
                // the gateway. OTB_MIN_CONC/OTB_MAX_CONC remain the escape hatch for narrowing a
                // debug run or widening this once the generator isn't thread-per-connection.
                //
                // The max is read from the host, not a chosen constant: a TCP connection needs a
                // unique (src ip, src port, dst ip, dst port), every window here drives one
                // destination, so simultaneous connections can't exceed this host's ephemeral source
                // port range (`/proc/sys/net/ipv4/ip_local_port_range`, ~28,000 by default) — past
                // that the rig hits EADDRNOTAVAIL, which used to be misread as the gateway refusing.
                // The orchestrator widens the port range before a run (run-on-ec2.sh); since the
                // ceiling is derived, that's the only number anyone has to move.
                min_conc: env_u32("OTB_MIN_CONC", 1),
                max_conc: env_u32("OTB_MAX_CONC", otb_engine::run::host_connection_ceiling()),
                measured_at: utc_stamp(),
                arch: std::env::var("BENCH_ARCH").unwrap_or_else(|_| "unknown".into()),
                // Same path as arch: the orchestrator knows the box shape, the box does not.
                hardware: std::env::var("BENCH_HARDWARE")
                    .ok()
                    .filter(|v| !v.trim().is_empty()),
                // Which commit produced this run: run-on-ec2.sh resolves it before the box exists,
                // since the box's own clone is a detached checkout and the binary is a release
                // download. Unset/empty is None, never the empty string, so a snapshot can say "not
                // traceable" rather than claim a commit named "".
                // Which mock took the readings: rig.sh fetched and hashed it on the box; passed
                // through rather than re-derived. Absent env means an absent block, not a fabricated one.
                rig_mock: std::env::var("OTB_RIG_MOCK_SHA256")
                    .ok()
                    .filter(|v| !v.is_empty())
                    .map(|sha| otb_engine::record::BinaryProvenance {
                        origin: std::env::var("OTB_RIG_MOCK_ORIGIN")
                            .ok()
                            .filter(|v| !v.is_empty()),
                        sha256: Some(sha),
                        asset_updated_at: std::env::var("OTB_RIG_MOCK_UPDATED_AT")
                            .ok()
                            .filter(|v| !v.is_empty()),
                    }),
                rig_release_url: std::env::var("OTB_RIG_URL").ok().filter(|v| !v.is_empty()),
                engine_stamp: std::env::var("BENCH_ENGINE_COMMIT")
                    .ok()
                    .filter(|c| !c.is_empty())
                    .map(|commit| otb_engine::record::EngineStamp {
                        commit,
                        // Anything other than exactly "0" is treated as dirty: an unreadable flag
                        // should mark the run unreproducible rather than claim a clean one.
                        dirty: std::env::var("BENCH_ENGINE_DIRTY").as_deref() != Ok("0"),
                    }),
            };
            // Launch it, if the manifest says how. No declared launch means a gateway someone else is
            // running (what `smoke` still does). A declared launch means the harness owns the
            // gateway's lifetime end to end, so a run can't silently measure a leftover container.
            let launched = match cfg.manifest.launch_spec(
                &gw_cores,
                cfg.mock_addr.port(),
                &gw_dir,
                Duration::from_secs(60),
                Duration::from_secs(2),
            ) {
                None => None,
                Some(Err(e)) => {
                    eprintln!("manifest {manifest_path} cannot be launched: {e}");
                    return ExitCode::FAILURE;
                }
                Some(Ok(spec)) => {
                    let mut launcher = otb_engine::launch::RealLauncher::default();
                    match otb_engine::launch::launch_default(&mut launcher, &spec) {
                        Ok(l) => {
                            println!(
                                "launched {} in {} attempt(s)",
                                spec.runtime.identity(),
                                l.attempts
                            );
                            // Commands, in order, now that it's up and before anything is measured. A
                            // gateway with no config file is configured via its own admin API after
                            // boot, so this is the only point that can happen, and it must finish
                            // before a probe decides what the gateway serves. A failure here stops
                            // the run: a half-configured gateway answering probes for a never-wired
                            // upstream is worse than one that never started.
                            //
                            // Run in the gateway's own directory, not wherever otb was invoked from:
                            // commands write/read relative paths (`> .minted-auth` matters here), and
                            // this directory is where the memory phase's replay will look too.
                            otb_engine::launch::set_commands_dir(gw_dir.clone());
                            for line in &cfg.manifest.commands {
                                match otb_engine::launch::run_line(line, Duration::from_secs(120)) {
                                    Ok(()) => println!("setup: {line}"),
                                    Err(why) => {
                                        eprintln!("setup command failed: {line}: {why}");
                                        let _ = otb_engine::supervise::stop_and_wait(
                                            &spec.runtime,
                                            spec.port,
                                            Duration::from_secs(15),
                                        );
                                        return ExitCode::FAILURE;
                                    }
                                }
                            }
                            // A gateway with no config file may mint its own credential rather than
                            // accept a declared one (e.g. an admin API that generates a random token
                            // server-side). `run_line` spawns a fresh `/bin/sh -c` per line
                            // (launch.rs), so an `export` doesn't survive between commands — only a
                            // file does. See `resolve_minted_auth`.
                            let minted = resolve_minted_auth(&gw_dir);
                            // The harness owning the lifetime is exactly when the memory phase
                            // restarts the gateway, so a per-boot credential would go stale mid-run.
                            // Refused here, before any measurement.
                            if let Some(why) = stale_minted_auth_refusal(minted.as_deref(), true) {
                                eprintln!("{}: {why}", cfg.manifest.name);
                                let _ = otb_engine::supervise::stop_and_wait(
                                    &spec.runtime,
                                    spec.port,
                                    Duration::from_secs(15),
                                );
                                return ExitCode::FAILURE;
                            }
                            if let Some(minted) = minted {
                                println!(
                                    "setup: using minted auth from {}",
                                    gw_dir.join(".minted-auth").display()
                                );
                                cfg.manifest.auth = minted;
                            }
                            Some(spec)
                        }
                        Err(e) => {
                            // The gateway never came up; stop rather than publish a grid of absences
                            // that would read as the gateway failing.
                            eprintln!("{} never became ready: {e}", spec.runtime.identity());
                            return ExitCode::FAILURE;
                        }
                    }
                }
            };

            let outcome = run_suite(&cfg, gw);

            // Stop what we started, whatever happened, and say so if it didn't stop: a gateway left
            // running holds the port/cores the next one needs, surfacing as that gateway's boot
            // failure instead of this one's.
            let mut stop_failed = false;
            if let Some(spec) = &launched {
                let stop = otb_engine::supervise::stop_and_wait(
                    &spec.runtime,
                    spec.port,
                    Duration::from_secs(15),
                );
                if let Some(why) = end_of_run_stop_failure(&spec.runtime.identity(), &stop) {
                    eprintln!("{why}");
                    stop_failed = true;
                }
            }

            match outcome {
                Ok(paths) => {
                    println!("wrote {}", paths.current.display());
                    println!("wrote {}", paths.historical.display());
                    // The snapshot is honest, but the box isn't clean: a zero exit would wrongly
                    // tell the orchestrator it may launch the next gateway here.
                    if stop_failed {
                        return ExitCode::FAILURE;
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("snapshot not written: {e}");
                    ExitCode::FAILURE
                }
            }
        }
        // Check a gateway's setup without running anything, and say everything that is wrong.
        Some("validate") => {
            use otb_engine::manifest::Manifest;
            let targets: Vec<std::path::PathBuf> = if args.len() > 1 {
                args[1..].iter().map(std::path::PathBuf::from).collect()
            } else {
                // No argument: check the whole field, which is what CI wants.
                let mut all: Vec<_> = std::fs::read_dir("gateways")
                    .into_iter()
                    .flatten()
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.join("definition.json").is_file())
                    .collect();
                all.sort();
                all
            };
            if targets.is_empty() {
                eprintln!("no gateways found. usage: otb validate [gateways/<name> ...]");
                return ExitCode::from(2);
            }

            let mut bad = 0usize;
            for dir in &targets {
                let name = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                match Manifest::load(dir) {
                    Err(e) => {
                        println!("{name}: FAIL");
                        println!("  {e}");
                        bad += 1;
                    }
                    Ok(m) => {
                        let problems = m.problems(dir);
                        if problems.is_empty() {
                            println!("{name}: ok");
                        } else {
                            println!("{name}: {} problem(s)", problems.len());
                            for p in &problems {
                                println!("  {p}");
                            }
                            bad += 1;
                        }
                    }
                }
            }
            println!();
            println!(
                "{} of {} gateways are ready to run",
                targets.len() - bad,
                targets.len()
            );
            if bad == 0 {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Some("version") => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        /* The commit this binary was built from, read back out of the binary itself.

           Boxes download the engine rather than build it, and the commit stamped in an artifact
           otherwise comes from an orchestrator env var recording only what was INTENDED to run —
           which can disagree with what was actually fetched. So the run asks the binary directly
           and refuses to measure on a mismatch. Empty output means the build couldn't establish
           its own commit; treat that as unverifiable, not as a match. */
        Some("engine-commit") => {
            println!("{}", env!("OTB_ENGINE_COMMIT"));
            ExitCode::SUCCESS
        }
        // The shell is handed an already-windowed file, so window over everything here to match.
        Some("plateau-check") => {
            let samples = read_samples();
            let steady = stats::plateau_check(
                &samples,
                f64::INFINITY,
                arg_f64(&args, 1, 1.0),
                arg_f64(&args, 2, 2.0),
            )
            .is_steady();
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

#[cfg(test)]
mod utc_stamp_tests {
    use super::utc_stamp;

    // A stamp with dashes where the time portion's colons belong parses as NaN in the site
    // generator's Date.parse, silently rendering measured_at as null.
    #[test]
    fn the_stamp_is_real_iso_8601_with_colons_in_the_time_portion() {
        let s = utc_stamp();
        assert!(
            regex_free_iso_shape(&s),
            "utc_stamp() produced {s:?}, which is not YYYY-MM-DDTHH:MM:SSZ"
        );
    }

    // No regex crate in this binary's dependencies - a tiny hand check is clearer than pulling one
    // in for a single call site.
    fn regex_free_iso_shape(s: &str) -> bool {
        let bytes = s.as_bytes();
        bytes.len() == 20
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes[10] == b'T'
            && bytes[13] == b':'
            && bytes[16] == b':'
            && bytes[19] == b'Z'
            && bytes
                .iter()
                .enumerate()
                .all(|(i, b)| matches!(i, 4 | 7 | 10 | 13 | 16 | 19) || b.is_ascii_digit())
    }
}

#[cfg(test)]
mod end_of_run_tests {
    use super::{end_of_run_stop_failure, stale_minted_auth_refusal};
    use otb_engine::supervise::SuperviseError;
    use std::time::Duration;

    // Regression test: the final stop's result used to be dropped with `let _`, so a gateway that
    // outlived the 15s budget kept the port/cores, surfacing only as the next gateway failing to boot.
    #[test]
    fn a_gateway_that_survives_the_stop_budget_is_reported_loudly() {
        let stop = Err(SuperviseError::StillHeld {
            port: 8080,
            waited: Duration::from_secs(16),
        });
        let said =
            end_of_run_stop_failure("gw-bench-4242", &stop).expect("a survivor is a failure");
        assert!(
            said.contains("gw-bench-4242"),
            "the message must name what is still running: {said}"
        );
        assert!(
            said.contains("8080"),
            "and the port it is still holding: {said}"
        );
        assert!(
            said.contains("NEXT gateway"),
            "and what it will break next: {said}"
        );
    }

    #[test]
    fn a_gateway_that_actually_stopped_reports_nothing() {
        assert_eq!(end_of_run_stop_failure("gw-bench-4242", &Ok(())), None);
    }

    // Minted auth is resolved once after the initial launch; a gateway that mints per boot would
    // hand out a new credential when the memory phase restarts it, and the second half of the grid
    // would read as the gateway refusing its own traffic.
    #[test]
    fn a_minting_gateway_the_harness_restarts_is_refused_before_it_is_measured() {
        let why = stale_minted_auth_refusal(Some("sk-minted-1"), true)
            .expect("this combination cannot be measured honestly");
        assert!(
            why.contains("stale"),
            "the refusal must name the defect: {why}"
        );
    }

    #[test]
    fn a_gateway_that_mints_nothing_is_measured_as_before() {
        assert_eq!(stale_minted_auth_refusal(None, true), None);
    }

    // Someone else's gateway, someone else's lifetime: nothing here restarts it, so its own
    // boot-minted credential stays valid for the whole run.
    #[test]
    fn a_minting_gateway_the_harness_never_restarts_is_fine() {
        assert_eq!(stale_minted_auth_refusal(Some("sk-minted-1"), false), None);
    }
}

#[cfg(test)]
mod minted_auth_tests {
    use super::resolve_minted_auth;

    // A throwaway directory per test so tests can run concurrently without colliding on
    // `.minted-auth`. No `tempfile` dep for one file.
    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "otb-minted-auth-test-{name}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // A gateway whose real credential is only known after boot gets that credential into the
    // probe, not the manifest's placeholder.
    #[test]
    fn a_minted_file_overrides_the_declared_auth() {
        let dir = scratch_dir("present");
        std::fs::write(dir.join(".minted-auth"), "sk-real-key-123\n").unwrap();
        assert_eq!(
            resolve_minted_auth(&dir),
            Some("sk-real-key-123".to_string())
        );
    }

    // No file, so the declared `auth` stands untouched.
    #[test]
    fn no_file_means_no_override() {
        let dir = scratch_dir("absent");
        assert_eq!(resolve_minted_auth(&dir), None);
    }

    // A command that touched the file but wrote nothing useful must not silently authenticate with
    // an empty bearer token — worse than falling back to the placeholder.
    #[test]
    fn a_whitespace_only_file_means_no_override() {
        let dir = scratch_dir("blank");
        std::fs::write(dir.join(".minted-auth"), "   \n\t\n").unwrap();
        assert_eq!(resolve_minted_auth(&dir), None);
    }

    #[test]
    fn surrounding_whitespace_is_trimmed() {
        let dir = scratch_dir("padded");
        std::fs::write(dir.join(".minted-auth"), "  sk-abc  \n").unwrap();
        assert_eq!(resolve_minted_auth(&dir), Some("sk-abc".to_string()));
    }
}
