// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// The engine binary. Subcommands mirror the shell functions one for one, taking the same input on
// stdin and printing the same thing on stdout, so a differential harness can run both
// implementations over generated inputs and diff them: parity that is executed catches more than
// parity that is only reviewed by eye.
//
// This is a separate crate target from the library (its own root, not lib.rs), so it needs its own
// copy of the same test-only unwrap/expect/panic exemption lib.rs documents.
#![cfg_attr(test, allow(clippy::panic, clippy::unwrap_used, clippy::expect_used))]

use std::io::Read;
use std::process::ExitCode;

use otb_engine::gen::{self, GenConfig};
use otb_engine::stats::{self, Sample, Verdict};
use std::time::Duration;

/// If `commands` left a minted credential at `<gw_dir>/.minted-auth`, its trimmed contents are what
/// every probe should authenticate with instead of the manifest's declared (placeholder) `auth`.
/// `None` for the absent, unreadable, or whitespace-only cases - every one of those means "nothing
/// was minted", so the declared `auth` stands.
fn resolve_minted_auth(gw_dir: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(gw_dir.join(".minted-auth")).ok()?;
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.to_string())
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

/// Real ISO-8601, colons and all - `measured_at`'s own contract (snapshot.rs's `write_snapshot`
/// derives the filesystem-safe historical filename by replacing ':' with '-' on exactly this
/// shape; its test fixtures were always written this way). A colon-less stamp here is not a
/// cosmetic difference: `Date.parse` in the site generator returns NaN on it, so every
/// `measured_at` this produced silently failed to parse and rendered as null on the board.
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
            // THE HEADERS COME FROM THE ENGINE THAT SPAWNED THIS, via the environment.
            //
            // They used to be hardcoded here as `authorization: Bearer dummy`, and that is a
            // data-invalidating defect rather than a placeholder: the in-process probe authenticated
            // with the manifest's real credential in the right per-dialect shape, every LOAD WINDOW
            // authenticated as the literal string "dummy" with a header name two of the six dialects
            // do not use, and any gateway whose declared auth was anything else passed its probe and
            // then failed 100% of every window. The absence that reached the artifact blamed the
            // SEARCH ("no probed concurrency passed the gate") for a credential fault of ours.
            //
            // Unset means NO headers, never a placeholder: see `loadgen::decode_headers` for why a
            // wrong credential is worse than none. The warning is because a hand-run `otb loadgen`
            // against an authenticating gateway would otherwise read as the gateway falling over.
            let headers = otb_engine::loadgen::decode_headers(
                std::env::var(otb_engine::loadgen::HEADERS_ENV).ok().as_deref(),
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
                model: args.get(3).cloned().unwrap_or_else(|| "gpt-4o-mini".into()),
                auth: "dummy".into(),
                dialects: vec![Dialect::Openai, Dialect::Anthropic, Dialect::Gemini],
                sweep_duration_s: 2,
                probe_timeout: Duration::from_secs(10),
                load_cores: std::env::var("LOADCORES").ok(),
                // `smoke` drives an already-running gateway and takes no manifest, so it has no
                // declared identity to measure memory against. An empty match resolves to nothing
                // and the memory group reports an absence naming it, which is the honest answer.
                static_headers: Vec::new(),
                egress_headers: Default::default(),
                runtime: otb_engine::manifest::Runtime::Native { proc_match: String::new() },
                // `smoke` runs against a target someone else started, so the harness does not own
                // its lifetime and must never restart it.
                // `smoke` drives a target at whatever path the dialect defines; it takes no manifest.
                declared_path: String::new(),
                cell_paths: Default::default(),
                // `smoke` takes no manifest, so there is no declared capability grid to consult:
                // every cell is probed, exactly as before this field existed.
                matrix: Vec::new(),
                matrix_note: String::new(),
                untestable_cells: Vec::new(),
                untestable_note: String::new(),
                relaunch: None,
            };
            println!("mock healthy: {}", otb_engine::run::mock_healthy(&cfg));
            for r in run_grid(&cfg, 4, 64) {
                // Print every metric the engine took, not a hand-picked one: the smoke's job is to
                // show what the engine actually produced, and a curated line would hide a group that
                // silently returned nothing.
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
                println!("{:<28} {:<12} {}", r.outcome.id.to_string(), format!("{:?}", r.outcome.served).chars().take(11).collect::<String>(), perf);
            }
            ExitCode::SUCCESS
        }
        // The whole suite for one gateway: probe the grid, sweep what is served, judge each peak
        // against the rig at the same operating point, and write the snapshot.
        Some("run") => {
            use otb_engine::manifest::Manifest;
            use otb_engine::suite::{run_suite, SuiteConfig};
            let Some(manifest_path) = args.get(1) else {
                eprintln!("usage: otb run <manifest.json> <gateway ip:port> <mock ip:port> [results_dir]");
                return ExitCode::from(2);
            };
            // A gateway is a DIRECTORY, not a file: definition.json plus whatever sidecars it has.
            // Passing the definition itself still works, so an operator can point at the file they
            // were reading a moment ago and get the same thing.
            let dir = {
                let p = std::path::Path::new(manifest_path);
                if p.is_dir() { p.to_path_buf() } else { p.parent().unwrap_or(p).to_path_buf() }
            };
            let manifest: Manifest = match Manifest::load(&dir) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::FAILURE;
                }
            };
            if let Err(e) = manifest.validate() {
                eprintln!("manifest {manifest_path} is incomplete: {e}");
                return ExitCode::FAILURE;
            }
            // THE CONFIG-NECESSITY GATE, at the only point where refusing still costs nothing.
            //
            // The board's fairness rule is that every gateway config is the bare minimum required to
            // run. A manifest that fails that has not earned a number, and finding out after the run
            // means the box-hours are already spent and the tempting fix is to publish anyway.
            //
            // Defaults are empty for now: there is no per-gateway source of "what this ships with out
            // of the box" in the tree yet, so the restated-default rule cannot fire. The structural
            // rules - an unusable key, a duplicate claim - do fire, and this is where they belong.
            let findings = otb_engine::config_lint::lint(&manifest, &otb_engine::config_lint::Defaults::new());
            for f in &findings {
                eprintln!("config lint: {}", f.message);
            }
            if otb_engine::config_lint::blocks(&findings) {
                eprintln!("manifest {manifest_path} does not meet the config-necessity standard; refusing to measure it");
                return ExitCode::FAILURE;
            }
            // The gateway's port is DECLARED in its definition, so the caller does not repeat it.
            // A caller-supplied port is a second spelling of one fact, and the run would drive
            // whatever answered on it - which is how a measurement ends up attributed to the wrong
            // gateway. An explicit address is still accepted for driving something already running.
            let Some(mk) = args.get(2).and_then(|a| a.parse::<std::net::SocketAddr>().ok()) else {
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
                        eprintln!("{} declares port {} which is not usable: {e}", manifest.name, manifest.port);
                        return ExitCode::FAILURE;
                    }
                },
            };
            let results_dir = args.get(3).cloned().unwrap_or_else(|| "results/snapshots".into());
            if let Err(e) = std::fs::create_dir_all(&results_dir) {
                eprintln!("cannot create {results_dir}: {e}");
                return ExitCode::FAILURE;
            }
            // The grid and the search range are overridable, not only for convenience: the full
            // default run is 36 cells x a peak search x a pinned child per rung, and an end-to-end
            // run that cannot be shrunk cannot be tested. Passing nothing here gets the same
            // defaults a field run uses.
            let raw_dialects = std::env::var("OTB_DIALECTS").ok();
            let dialects = match otb_engine::ingress::dialects_from(raw_dialects.as_deref()) {
                Ok(d) => d,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::from(2);
                }
            };
            let env_u32 = |k: &str, d: u32| std::env::var(k).ok().and_then(|v| v.parse().ok()).unwrap_or(d);
            let gw_dir = dir.clone();
            let gw_cores = std::env::var("OTB_GW_CORES").unwrap_or_else(|_| "0-3".into());

            // RENDER THE CONFIG BEFORE LAUNCHING. Seven of the containers mount a file the harness
            // writes and every source build points at one; launching before it exists produces a
            // gateway that starts and dies, which reads as the gateway being broken.
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
                // THE ENGINE'S OWN DEFAULT, not an orchestrator setting: a real field run must
                // search wide enough to find ANY gateway's true peak, and a caller forgetting to
                // export an override should get the wide range, not a narrow one silently clipping
                // a fast gateway's ceiling. [512] was too narrow to find tensorzero's true peak on
                // the box that measured the field with it unset - the search reported "still rising
                // at the top of the range" rather than a number, for a gateway fast enough to have a
                // real answer. OTB_MIN_CONC/OTB_MAX_CONC stay as the escape hatch for narrowing a
                // debug run (matches OTB_DIALECTS's own "unset means real run" convention below).
                min_conc: env_u32("OTB_MIN_CONC", 1),
                max_conc: env_u32("OTB_MAX_CONC", 65536),
                measured_at: utc_stamp(),
                arch: std::env::var("BENCH_ARCH").unwrap_or_else(|_| "unknown".into()),
                // WHICH COMMIT PRODUCED THIS RUN. run-on-ec2.sh resolves it before the box exists
                // and exports it, because the box cannot work it out for itself: its clone is a
                // detached checkout and this binary arrived as a release download, so neither the
                // tree nor the executable knows the revision it came from.
                //
                // An unset or empty variable is None, not the empty string: a snapshot must be able
                // to say "this run cannot be traced" rather than claim a commit named "".
                // WHICH MOCK TOOK THE READINGS. rig.sh on the box fetched and hashed it; these are
                // passed through rather than re-derived, because the engine never sees the fetch.
                // Absent env means an absent block, never a fabricated one.
                rig_mock: std::env::var("OTB_RIG_MOCK_SHA256")
                    .ok()
                    .filter(|v| !v.is_empty())
                    .map(|sha| otb_engine::record::BinaryProvenance {
                        origin: std::env::var("OTB_RIG_MOCK_ORIGIN").ok().filter(|v| !v.is_empty()),
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
                        // Anything that is not exactly "0" is treated as dirty. A run from a
                        // modified tree is not identified by its commit, and the safe default for
                        // an unreadable flag is to mark it unreproducible rather than to claim it.
                        dirty: std::env::var("BENCH_ENGINE_DIRTY").as_deref() != Ok("0"),
                    }),
            };
            // LAUNCH IT, if the manifest says how.
            //
            // A manifest that declares no launch describes a gateway someone else is running, which
            // is what every run did until now and is what the smoke against an already-up target
            // still does. Declaring a launch means the harness owns the gateway's lifetime: it starts
            // it, measures it, and stops it, so a run cannot silently measure a container left over
            // from a previous one.
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
                            println!("launched {} in {} attempt(s)", spec.runtime.identity(), l.attempts);
                            // COMMANDS, in order, now that it is up and before anything is measured.
                            //
                            // A gateway with no config file is configured through its own admin API
                            // after it boots, so this is the only point at which that can happen: it
                            // needs the process answering, and it must be finished before a probe
                            // decides what the gateway does or does not serve.
                            //
                            // A failure here stops the run. A half-configured gateway is worse than
                            // one that never started: it answers probes, and publishes a verdict for
                            // an upstream that was never wired up.
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
                            // A GATEWAY WITH NO CONFIG FILE MAY MINT ITS OWN CREDENTIAL RATHER THAN
                            // LET ONE BE DECLARED. one-api's admin API generates a random token
                            // server-side and discards any client-supplied one, so no `commands`
                            // line can make the manifest's static `auth` valid by asking for it by
                            // name. `run_line` spawns a fresh `/bin/sh -c` per line (launch.rs), so
                            // an `export` in one command is invisible to the next and to this
                            // process - the only thing that survives across those separate
                            // invocations is a file. See `resolve_minted_auth`.
                            if let Some(minted) = resolve_minted_auth(&gw_dir) {
                                println!("setup: using minted auth from {}", gw_dir.join(".minted-auth").display());
                                cfg.manifest.auth = minted;
                            }
                            Some(spec)
                        }
                        Err(e) => {
                            // The gateway never came up. Publishing a grid of absences against a
                            // gateway that was never running would read as the gateway failing, so
                            // this stops rather than measuring nothing and calling it a result.
                            eprintln!("{} never became ready: {e}", spec.runtime.identity());
                            return ExitCode::FAILURE;
                        }
                    }
                }
            };

            let outcome = run_suite(&cfg, gw);

            // Stop what we started, whatever happened. A gateway left running holds the port and the
            // cores the NEXT gateway needs, and the failure that causes looks like the next gateway
            // refusing to boot.
            if let Some(spec) = &launched {
                let _ = otb_engine::supervise::stop_and_wait(&spec.runtime, spec.port, Duration::from_secs(15));
            }

            match outcome {
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
                let name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
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
            println!("{} of {} gateways are ready to run", targets.len() - bad, targets.len());
            if bad == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE }
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

#[cfg(test)]
mod utc_stamp_tests {
    use super::utc_stamp;

    // THE CASE THAT WAS BROKEN: a stamp with dashes where the time portion's colons belong parses
    // as NaN in the site generator's Date.parse, so every gateway silently rendered measured_at as
    // null. A real ISO-8601 shape is the whole contract this function exists to uphold.
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
            && bytes.iter().enumerate().all(|(i, b)| {
                matches!(i, 4 | 7 | 10 | 13 | 16 | 19) || b.is_ascii_digit()
            })
    }
}

#[cfg(test)]
mod minted_auth_tests {
    use super::resolve_minted_auth;

    // A tiny throwaway directory per test, so tests can run concurrently without treading on each
    // other's `.minted-auth`. Not `tempfile`: this crate takes no dev-dependency for one file.
    fn scratch_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("otb-minted-auth-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // THE CASE THIS EXISTS FOR: a gateway whose real credential can only be known after it boots
    // gets that credential into the probe, not the manifest's placeholder.
    #[test]
    fn a_minted_file_overrides_the_declared_auth() {
        let dir = scratch_dir("present");
        std::fs::write(dir.join(".minted-auth"), "sk-real-key-123\n").unwrap();
        assert_eq!(resolve_minted_auth(&dir), Some("sk-real-key-123".to_string()));
    }

    // TWELVE OF THIRTEEN ENTRANTS: no file, so the declared `auth` stands untouched.
    #[test]
    fn no_file_means_no_override() {
        let dir = scratch_dir("absent");
        assert_eq!(resolve_minted_auth(&dir), None);
    }

    // A command that ran but wrote nothing useful (a failed mint that still touched the file, an
    // empty redirect) must not silently authenticate with an empty bearer token - that is a
    // different, worse failure mode than "kept the placeholder".
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
