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
use otb_engine::stats::{self, Sample};
use std::time::Duration;

/// If `commands` left a minted credential at `<gw_dir>/.minted-auth`, its trimmed contents are what
/// every probe should authenticate with instead of the manifest's declared (placeholder) `auth`.
/// `Ok(None)` for the absent and whitespace-only cases - both mean "nothing was minted", so the
/// declared `auth` stands.
///
/// A FILE THAT EXISTS BUT CANNOT BE READ IS AN ERROR, not "nothing was minted". Something was minted
/// and we could not read it: folding that into the absent case skipped the stale-credential refusal
/// and authenticated every request with the placeholder, publishing a grid of 401s as the gateway's
/// own failures.
fn resolve_minted_auth(gw_dir: &std::path::Path) -> Result<Option<String>, String> {
    let path = gw_dir.join(".minted-auth");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(format!(
                "{} exists but could not be read ({e}), so the credential the gateway minted is unknown",
                path.display()
            ))
        }
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    Ok(Some(trimmed.to_string()))
}

/// WHY A GATEWAY THAT MINTS ITS OWN CREDENTIAL CANNOT BE MEASURED THROUGH A RESTART.
///
/// `resolve_minted_auth` runs ONCE, right after the initial launch, and the credential it finds is
/// copied into the config every later request authenticates with. The memory phase then stops and
/// relaunches the gateway (`run::restart_to_rest`), replaying the same `commands`: a gateway that
/// mints a fresh credential per boot mints a new one there, and every request after that point
/// carries a token the gateway no longer accepts. The symptom is not an error, it is a grid of
/// failures attributed to the gateway.
///
/// The engine cannot re-resolve it from here (the restart happens deep inside the grid, against a
/// credential the run config captured by value), so the honest move is to refuse the combination
/// rather than publish a run whose second half authenticated with a dead token. Returns the refusal
/// to print, or `None` when there is nothing to refuse.
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
/// The final `stop_and_wait` used to be discarded with `let _`. A gateway that outlives the stop
/// budget keeps the port and the cores the NEXT gateway needs, so the failure surfaces one gateway
/// later as that gateway "never becoming ready" - the run that caused it having already reported
/// success.
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

/// Stop a gateway this run launched and is abandoning before measuring it, and SAY SO when it would
/// not stop. These paths used to discard the stop's result, so a survivor's only report was the next
/// gateway on the box failing to boot.
fn stop_abandoned(
    spec: &otb_engine::launch::LaunchSpec,
    launcher: &mut otb_engine::launch::RealLauncher,
) {
    let stop =
        otb_engine::supervise::stop_and_wait(&spec.runtime, spec.port, Duration::from_secs(15));
    if let Some(why) = end_of_run_stop_failure(&spec.runtime.identity(), &stop) {
        eprintln!("{why}");
    }
    launcher.reap_previous_native_child();
}

fn usage() -> ExitCode {
    eprintln!(
        "otb {}\n\n\
         \x20 run <gateway dir> <mock ip:port> [results_dir] [sweep_s]   measure one gateway and write its snapshot\n\
         \x20 validate [gateways/<name> ...]          check gateway setups without running anything\n\
         \x20 loadgen <ip:port> <path> <concurrency> <duration_s> [body]   one load window, prints a stats line\n\
         \x20 smoke <gateway ip:port> <mock ip:port> [model]   probe and sweep an already-running gateway\n\
         \x20 engine-commit                           prints the commit this binary was built from\n\
         \x20 version\n\n\
         Samples arrive on stdin as \"<t_s> <mib>\" per line, matching the shell's series files.\n\n\
         \x20 plateau-check <trend_pct> <range_pct>   prints 1 for steady, 0 otherwise (mirrors plateau_check)\n\
         \x20 growth-rate                             prints MiB/min to 3dp, or nothing (mirrors plateau_growth_rate)\n\
         \x20 window <window_s>                       prints the trailing window (mirrors plateau_window)\n",
        env!("CARGO_PKG_VERSION")
    );
    ExitCode::from(2)
}

/// A usage error with its reason, on stderr, exit 2.
fn usage_error(why: &str) -> ExitCode {
    eprintln!("{why}");
    ExitCode::from(2)
}

/// The stdin series. Malformed LINES are skipped (the shell's own contract, and the CLI contract tests
/// pin it); a stdin that could not be read at all is an error, never an empty series that prints a
/// verdict about nothing with exit status 0.
fn read_samples() -> Result<Vec<Sample>, String> {
    let mut buf = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut buf) {
        return Err(format!("could not read the series from stdin: {e}"));
    }
    Ok(buf
        .lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let t = it.next()?.parse::<f64>().ok()?;
            let v = it.next()?.parse::<f64>().ok()?;
            Some(Sample { t_s: t, mib: v })
        })
        .collect())
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
    utc_stamp_at(secs)
}

/// The stamp for `secs` since the Unix epoch. Split from the clock so the calendar arithmetic can be
/// checked against known dates.
fn utc_stamp_at(secs: u64) -> String {
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
/// set one (run-on-ec2.sh already has a `RUN_ID` it tags its boxes with), otherwise this process's
/// pid, which is all two runs sharing a box need in order to stop naming each other's containers.
fn run_scope() -> String {
    std::env::var("OTB_RUN_ID")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| std::process::id().to_string())
}

/// A positional argument: absent or empty takes `default`, present must parse.
///
/// A malformed argument used to fall back to the default in silence, so `otb loadgen ... abc` ran a
/// c=1 window and printed a normal stats line, and `otb run ... results abc` swept with 6-second
/// windows - each published as the run that was asked for. Empty is treated as absent because the
/// orchestrator exports unset settings as empty strings.
fn arg_or<T: std::str::FromStr>(args: &[String], i: usize, default: T) -> Result<T, String> {
    parse_or(args.get(i).map(String::as_str), default).map_err(|raw| {
        format!(
            "argument {i} ({raw:?}) is not a valid {}",
            std::any::type_name::<T>()
        )
    })
}

/// The shared rule behind `arg_or` and the `OTB_*` environment settings: `None` or blank is the
/// default, anything else must parse. `Err` carries the raw text that did not.
fn parse_or<T: std::str::FromStr>(raw: Option<&str>, default: T) -> Result<T, String> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(default),
        Some(v) => v.parse().map_err(|_| v.to_string()),
    }
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
                    eprintln!(
                        "usage: otb loadgen <ip:port> <path> <concurrency> <duration_s> [body]"
                    );
                    return ExitCode::from(2);
                }
            };
            let path = args.get(2).cloned().unwrap_or_else(|| "/".into());
            let (conc, dur): (u32, u64) = match (arg_or(&args, 3, 1), arg_or(&args, 4, 5)) {
                (Ok(c), Ok(d)) => (c, d),
                (Err(e), _) | (_, Err(e)) => {
                    return usage_error(&format!(
                        "otb loadgen: {e}\nusage: otb loadgen <ip:port> <path> <concurrency> <duration_s> [body]"
                    ))
                }
            };
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
            // A set-but-broken variable prints NOTHING on stdout and exits non-zero: the parent then
            // reads an absent window, never one driven without the headers it declared.
            let headers = match otb_engine::loadgen::decode_headers(
                std::env::var(otb_engine::loadgen::HEADERS_ENV)
                    .ok()
                    .as_deref(),
            ) {
                Ok(h) => h,
                Err(why) => {
                    eprintln!("loadgen: {why}");
                    return ExitCode::FAILURE;
                }
            };
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
                // `smoke` takes no manifest, so there is nothing to vary the egress column by.
                egress_models: Default::default(),
                model: args.get(3).cloned().unwrap_or_else(|| "gpt-4o-mini".into()),
                auth: "dummy".into(),
                dialects: vec![Dialect::Openai, Dialect::Anthropic, Dialect::Gemini],
                sweep_duration_s: 2,
                probe_timeout: Duration::from_secs(10),
                load_cores: std::env::var("LOADCORES").ok(),
                // `smoke` drives an ALREADY-RUNNING gateway that this process did not pin, so there
                // is no core list it can honestly claim. Empty means the cost window reports
                // utilisation as absent rather than measuring some other process's cores and
                // labelling the result this gateway's.
                gw_cores: String::new(),
                // `smoke` drives an already-running gateway and takes no manifest, so it has no
                // declared identity to measure memory against. An empty match resolves to nothing -
                // enforced in `supervise::select_matches`, because the `pgrep -f ""` this used to be
                // matched EVERY process on the box and could publish init's tree as the gateway's
                // RSS - and the memory group reports an absence naming it, which is the honest answer.
                static_headers: Vec::new(),
                egress_headers: Default::default(),
                runtime: otb_engine::manifest::Runtime::Native {
                    proc_match: String::new(),
                },
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
                relaunch_commands: Vec::new(),
                relaunch_launcher: Default::default(),
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
        // The whole suite for one gateway: probe the grid, sweep what is served, judge each peak
        // against the rig at the same operating point, and write the snapshot.
        Some("run") => {
            use otb_engine::manifest::Manifest;
            use otb_engine::suite::{run_suite, SuiteConfig};
            let Some(manifest_path) = args.get(1) else {
                eprintln!("usage: otb run <gateway dir> <mock ip:port> [results_dir] [sweep_s]");
                return ExitCode::from(2);
            };
            // A gateway is a DIRECTORY, not a file: definition.json plus whatever sidecars it has.
            // Passing the definition itself still works, so an operator can point at the file they
            // were reading a moment ago and get the same thing.
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
            // BIND EVERY CONTAINER THIS INVOCATION CREATES TO THIS INVOCATION. Without it the
            // container name is the manifest's alone, identical on every run of this gateway on this
            // box, so a second run's boot-retry `docker rm -f` deletes the FIRST run's container in
            // the middle of its measurement. `OTB_RUN_ID` lets the orchestrator use the same run id
            // it tags its boxes with; a bare pid is enough to separate two runs on one box.
            manifest.runtime = manifest.runtime.scoped_to_run(&run_scope());
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
            let findings =
                otb_engine::config_lint::lint(&manifest, &otb_engine::config_lint::Defaults::new());
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
            // Unset or empty takes the default (the orchestrator exports an unset override as an empty
            // string); anything else must be a number. A typo here used to restore the default in
            // silence - `OTB_MAX_CONC=1o24` quietly searched to the host ceiling instead.
            let env_u32 = |k: &str, d: u32| -> Result<u32, String> {
                parse_or(std::env::var(k).ok().as_deref(), d)
                    .map_err(|raw| format!("{k}={raw:?} is not a whole number"))
            };
            let (min_conc, max_conc) = match (
                env_u32("OTB_MIN_CONC", 1),
                env_u32("OTB_MAX_CONC", otb_engine::run::host_connection_ceiling()),
            ) {
                (Ok(lo), Ok(hi)) => (lo, hi),
                (Err(e), _) | (_, Err(e)) => return usage_error(&e),
            };
            // A window shorter than a second rounds to a zero-length window, which measures nothing.
            let sweep_duration_s = match arg_or::<f64>(&args, 4, 6.0) {
                Ok(s) if s.is_finite() && s >= 1.0 => s as u64,
                Ok(s) => {
                    return usage_error(&format!(
                        "sweep_s {s} is not a usable window length (whole seconds, at least 1)"
                    ))
                }
                Err(e) => return usage_error(&e),
            };
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
                sweep_duration_s,
                load_cores: std::env::var("LOADCORES").ok(),
                // THE ENGINE'S OWN DEFAULT, not an orchestrator setting: a real field run must
                // search wide enough to find ANY gateway's true peak, and a caller forgetting to
                // export an override should get the wide range, not a narrow one silently clipping
                // a fast gateway's ceiling. OTB_MIN_CONC/OTB_MAX_CONC stay as the escape hatch for
                // narrowing a debug run (matches OTB_DIALECTS's own "unset means real run"
                // convention).
                //
                // THE CEILING IS READ FROM THE HOST, not chosen as a constant.
                //
                // 4096 was picked when the load generator was thread-per-connection, and raising it
                // to a bigger constant only moved the arbitrary number. The real bound is physical: a
                // TCP connection needs a unique (src ip, src port, dst ip, dst port), and every
                // window here drives ONE destination, so simultaneous connections cannot exceed this
                // host's ephemeral source ports. On a default Linux that is about 28,000 - already
                // below the 65536 a wider constant would ask for - and asking past it raises
                // EADDRNOTAVAIL, which is the rig running out and used to be counted as the gateway
                // refusing.
                //
                // So the engine reads `/proc/sys/net/ipv4/ip_local_port_range` and caps itself at
                // what the host can actually do. The orchestrator widens that range before a run
                // (run-on-ec2.sh), and because the ceiling is derived rather than declared, widening
                // it is the only thing anyone has to change - there is no second number to move.
                min_conc,
                max_conc,
                measured_at: utc_stamp(),
                arch: std::env::var("BENCH_ARCH").unwrap_or_else(|_| "unknown".into()),
                // Same path as arch: the orchestrator knows the box shape, the box does not.
                hardware: std::env::var("BENCH_HARDWARE")
                    .ok()
                    .filter(|v| !v.trim().is_empty()),
                // WHICH MOCK TOOK THE READINGS. rig.sh on the box fetched and hashed it; these are
                // passed through rather than re-derived, because the engine never sees the fetch.
                // Absent env means an absent block, never a fabricated one.
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
                // WHICH COMMIT PRODUCED THIS RUN. run-on-ec2.sh resolves it before the box exists
                // and exports it, because the box cannot work it out for itself: its clone is a
                // detached checkout and this binary arrived as a release download, so neither the
                // tree nor the executable knows the revision it came from.
                //
                // An unset or empty variable is None, not the empty string: a snapshot must be able
                // to say "this run cannot be traced" rather than claim a commit named "".
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
                            println!(
                                "launched {} in {} attempt(s)",
                                spec.runtime.identity(),
                                l.attempts
                            );
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
                            // IN THE GATEWAY'S OWN DIRECTORY, not wherever otb was invoked from: a
                            // line transcribed from a gateway's docs writes and reads relative paths
                            // (`> .minted-auth` is the one that already matters), and only this
                            // directory is where anything looks for them. Declared once here so the
                            // memory phase's replay of the same commands lands in the same place.
                            otb_engine::launch::set_commands_dir(gw_dir.clone());
                            for line in &cfg.manifest.commands {
                                match otb_engine::launch::run_line(line, Duration::from_secs(120)) {
                                    Ok(()) => println!("setup: {line}"),
                                    Err(why) => {
                                        eprintln!("setup command failed: {line}: {why}");
                                        stop_abandoned(&spec, &mut launcher);
                                        return ExitCode::FAILURE;
                                    }
                                }
                            }
                            // A GATEWAY WITH NO CONFIG FILE MAY MINT ITS OWN CREDENTIAL RATHER THAN
                            // LET ONE BE DECLARED. One entrant's admin API generates a random token
                            // server-side and discards any client-supplied one, so no `commands`
                            // line can make the manifest's static `auth` valid by asking for it by
                            // name. `run_line` spawns a fresh `/bin/sh -c` per line (launch.rs), so
                            // an `export` in one command is invisible to the next and to this
                            // process - the only thing that survives across those separate
                            // invocations is a file. See `resolve_minted_auth`.
                            let minted = match resolve_minted_auth(&gw_dir) {
                                Ok(minted) => minted,
                                Err(why) => {
                                    eprintln!("{}: {why}", cfg.manifest.name);
                                    stop_abandoned(&spec, &mut launcher);
                                    return ExitCode::FAILURE;
                                }
                            };
                            // The harness owning the lifetime is exactly the condition under which
                            // the memory phase restarts the gateway, so a per-boot credential would
                            // go stale mid-run. Refused here, before a single measurement, rather
                            // than published as the gateway failing every cell after the restart.
                            if let Some(why) = stale_minted_auth_refusal(minted.as_deref(), true) {
                                eprintln!("{}: {why}", cfg.manifest.name);
                                stop_abandoned(&spec, &mut launcher);
                                return ExitCode::FAILURE;
                            }
                            if let Some(minted) = minted {
                                println!(
                                    "setup: using minted auth from {}",
                                    gw_dir.join(".minted-auth").display()
                                );
                                cfg.manifest.auth = minted;
                            }
                            // The launcher travels with the spec: it holds a native gateway's
                            // `Child`, and only it can reap that process once the run stops it.
                            Some((spec, launcher))
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
            // AND SAY SO IF IT DID NOT STOP. Discarding this result let a gateway that outlived the
            // budget keep the port and the cores, and the only report of it was the NEXT gateway
            // failing to boot.
            let mut stop_failed = false;
            if let Some((spec, mut launcher)) = launched {
                let stop = otb_engine::supervise::stop_and_wait(
                    &spec.runtime,
                    spec.port,
                    Duration::from_secs(15),
                );
                if let Some(why) = end_of_run_stop_failure(&spec.runtime.identity(), &stop) {
                    eprintln!("{why}");
                    stop_failed = true;
                }
                // Reap the native child this process spawned: a killed child that is never waited on
                // stays in the process table until this process exits.
                launcher.reap_previous_native_child();
            }

            match outcome {
                Ok(paths) => {
                    println!("wrote {}", paths.current.display());
                    println!("wrote {}", paths.historical.display());
                    // The snapshot is written and honest; the BOX is not clean. A zero exit here
                    // would tell the orchestrator it may launch the next gateway on this box.
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
                // EVERY DIRECTORY IS A GATEWAY, and one without a loadable definition is a FAIL below,
                // not a directory skipped in silence: a misnamed or deleted definition.json used to
                // drop the entrant from the count while the summary still said "N of N are ready".
                let entries = match std::fs::read_dir("gateways") {
                    Ok(entries) => entries,
                    Err(e) => {
                        eprintln!("cannot list gateways/: {e}");
                        return ExitCode::FAILURE;
                    }
                };
                let mut all = Vec::new();
                for entry in entries {
                    match entry {
                        Ok(e) if e.path().is_dir() => all.push(e.path()),
                        Ok(_) => {}
                        Err(e) => {
                            eprintln!("cannot list gateways/: {e}");
                            return ExitCode::FAILURE;
                        }
                    }
                }
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
        /* THE COMMIT THIS BINARY WAS BUILT FROM, read back out of the binary itself.

        The boxes do not build the engine, they download it, and the commit recorded in every
        artifact came from an environment variable the orchestrator set from its own checkout.
        That stamp said what the operator INTENDED to run. On 2026-08-03 a snapshot was stamped
        0ce7a907 and measured by a binary containing no line of it - the fixes were on a branch,
        the release only rebuilds on a push to main, and the boxes fetched the previous artifact.

        So the run asks the binary, and refuses to measure when the answer disagrees with the
        commit it is about to stamp. Empty output means the build could not establish its own
        commit, which the caller must treat as unverifiable rather than as a match. */
        Some("build-commit") => {
            println!("{}", env!("OTB_SOURCE_COMMIT"));
            ExitCode::SUCCESS
        }
        Some("engine-commit") => {
            println!("{}", env!("OTB_ENGINE_COMMIT"));
            ExitCode::SUCCESS
        }
        // The shell is handed an ALREADY-WINDOWED file, so window over everything here to match.
        Some("plateau-check") => {
            let (samples, trend, range) =
                match (read_samples(), arg_or(&args, 1, 1.0), arg_or(&args, 2, 2.0)) {
                    (Ok(s), Ok(t), Ok(r)) => (s, t, r),
                    (Err(e), _, _) | (_, Err(e), _) | (_, _, Err(e)) => return usage_error(&e),
                };
            let steady = stats::plateau_check(&samples, f64::INFINITY, trend, range).is_steady();
            println!("{}", u8::from(steady));
            ExitCode::SUCCESS
        }
        // %.3f matches plateau_growth_rate's own printf, so a diff compares numbers rather than
        // float formatting. An unmeasurable rate prints nothing, exactly as the shell's awk exits.
        Some("growth-rate") => {
            let samples = match read_samples() {
                Ok(s) => s,
                Err(e) => return usage_error(&e),
            };
            if let Some(r) = stats::growth_rate(&samples).copied() {
                println!("{r:.3}");
            }
            ExitCode::SUCCESS
        }
        Some("window") => {
            let (samples, window_s) = match (read_samples(), arg_or(&args, 1, 60.0)) {
                (Ok(s), Ok(w)) => (s, w),
                (Err(e), _) | (_, Err(e)) => return usage_error(&e),
            };
            for s in stats::window(&samples, window_s) {
                println!("{} {}", s.t_s, s.mib);
            }
            ExitCode::SUCCESS
        }
        _ => usage(),
    }
}

#[cfg(test)]
#[path = "tests/otb_utc_stamp_tests.rs"]
mod utc_stamp_tests;

#[cfg(test)]
#[path = "tests/otb_end_of_run_tests.rs"]
mod end_of_run_tests;

#[cfg(test)]
#[path = "tests/otb_minted_auth_tests.rs"]
mod minted_auth_tests;

#[cfg(test)]
#[path = "tests/otb_arg_tests.rs"]
mod arg_tests;
