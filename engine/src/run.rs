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
use crate::metric;
use crate::probe::{persistent_transient_verdict, Observation, Verdict};
use crate::search::{self, Probe, Sample};

pub struct RunConfig {
    pub gateway_addr: SocketAddr,
    pub mock_addr: SocketAddr,
    pub model: String,
    /// Per-egress model names. See `Manifest::egress_models`; read through `model_for`, never
    /// directly, so no call site can accidentally send the diagonal's model to a translation cell.
    pub egress_models: std::collections::BTreeMap<String, String>,
    pub auth: String,
    /// Dialects to walk. Both axes use the same set: ingress is what the client speaks, egress is
    /// what the upstream speaks.
    pub dialects: Vec<Dialect>,
    pub sweep_duration_s: u64,
    pub probe_timeout: Duration,
    /// CPU list the load generator is pinned to, e.g. "4-9". None only in tests.
    pub load_cores: Option<String>,
    /// The CPU list the GATEWAY is pinned to, exactly as taskset was given it.
    ///
    /// Carried so the cost window can report utilisation of those cores and nothing else: the box
    /// also runs the mock and the load generator on their own cores, and a machine-wide figure would
    /// blend three processes and describe none of them. It reads the SAME declaration the pinning
    /// used rather than a second list that could drift from it.
    pub gw_cores: String,
    /// Headers this gateway needs on every request, whatever the cell.
    pub static_headers: Vec<(String, String)>,
    /// Headers that select an egress column, keyed by dialect. Empty for a gateway that routes by
    /// config rather than by header.
    pub egress_headers: std::collections::BTreeMap<String, Vec<(String, String)>>,
    /// The gateway's declared identity, so the memory readers can find its process tree. The SAME
    /// value the launcher's --name and the stop path take: there is no second name for a reader to
    /// disagree with.
    pub runtime: crate::manifest::Runtime,
    /// The gateway's declared ingress path, when it differs from the dialect's standard one (e.g. a
    /// compatible API mounted under a prefix). Ignoring this previously made served gateways appear
    /// to serve nothing (404 on every cell). Applies only to the one dialect whose standard path it
    /// ends with; every other dialect keeps its own.
    pub declared_path: String,
    /// Per-cell overrides, keyed `"<ingress>>egress"`. See `Manifest::cell_paths`.
    pub cell_paths: std::collections::BTreeMap<String, String>,
    /// The gateway's declared capability grid and the cells the rig cannot pose. See
    /// `Manifest::matrix` / `Manifest::untestable`. Empty `matrix` means undeclared: every cell is
    /// probed, unchanged from before this field existed.
    pub matrix: Vec<String>,
    pub matrix_note: String,
    pub untestable_cells: Vec<String>,
    pub untestable_note: String,
    /// How to restart the gateway to rest, so the memory group can read an idle RSS. `None` when
    /// the harness does not own the gateway's lifetime; memory then publishes idle as ABSENT rather
    /// than a reading taken under load — see `Memory::measure`.
    pub relaunch: Option<crate::launch::LaunchSpec>,
    /// The manifest's post-boot `commands`, replayed on every restart. Config applied via admin API
    /// after boot lives in the container's writable layer, which a `docker rm -f` stop destroys;
    /// skipping replay would relaunch an unconfigured gateway that fails every metric after restart.
    pub relaunch_commands: Vec<String>,
    /// The one launcher that owns this gateway's native child across every restart. A per-call
    /// throwaway launcher drops its `Child` when it returns, so the next restart's `pkill` kills a
    /// process nothing can `wait()` on, leaking a zombie entry per served cell. Sharing the launcher
    /// lets it reap what it spawned (see `RealLauncher::reap_previous_native_child`). Present even
    /// when `relaunch` is `None`, just unused.
    pub relaunch_launcher: std::sync::Mutex<crate::launch::RealLauncher>,
}

/// Every header one request carries: how this INGRESS dialect authenticates, then whatever the
/// gateway needs to select this EGRESS column. Auth headers come from `Dialect` (protocol-level,
/// same across gateways); routing headers come from the manifest, keyed by column (gateway-level).
/// Where to send this dialect's probe: the gateway's declared path when it is a longer form of this
/// dialect's standard one, otherwise the standard.
/// The model name this cell must send to reach its egress column. Callers must go through here
/// rather than `cfg.model`: most gateways pick the upstream from the model name, so a fixed model
/// would reach one upstream while claiming six egress columns were exercised. Falls back to the
/// declared `model` when the manifest names nothing for this column.
pub fn model_for(cfg: &RunConfig, egress: &str) -> String {
    cfg.egress_models
        .get(egress)
        .cloned()
        .unwrap_or_else(|| cfg.model.clone())
}

pub fn path_for(cfg: &RunConfig, ingress: Dialect, egress: &str) -> String {
    let standard = ingress.path(&model_for(cfg, egress));
    // Most specific first: a cell's own path, then the gateway's declared one, then the standard.
    if let Some(p) = cfg
        .cell_paths
        .get(&format!("{}>{}", ingress.as_str(), egress))
    {
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

/// The exact header list one cell is driven with: the dialect's own credential headers, then the
/// manifest's always-on headers, then the ones that select this egress column.
///
/// Ledger RIG-12: one header per name on the wire, case-insensitively deduped, with the dialect's
/// own credential header always winning over a manifest-declared duplicate (e.g. `litellm-rust`
/// declares an `Authorization` header colliding with several ingress dialects' bearer headers).
/// HTTP does not define which of two same-name headers a server honours, so a duplicate risks
/// silently authenticating as the wrong identity while still publishing a clean number. The
/// dialect's header wins because `cfg.auth` is the credential the harness can actually name.
/// Dropped rather than refused at load (refusing would halt the whole benchmark); collisions are
/// reported via `Manifest::rig_owned_headers_declared` / `otb validate`.
pub(crate) fn headers_for(
    cfg: &RunConfig,
    ingress: Dialect,
    egress: &str,
) -> Vec<(String, String)> {
    let mut out = ingress.auth_headers(&cfg.auth);
    // Header names compared case-insensitively per HTTP semantics.
    let rig_owned: Vec<String> = out.iter().map(|(n, _)| n.to_ascii_lowercase()).collect();
    let push = |out: &mut Vec<(String, String)>, (n, v): (String, String)| {
        if rig_owned.contains(&n.to_ascii_lowercase()) {
            return;
        }
        out.push((n, v));
    };
    for h in cfg.static_headers.iter().cloned() {
        push(&mut out, h);
    }
    if let Some(extra) = cfg.egress_headers.get(egress) {
        for h in extra.iter().cloned() {
            push(&mut out, h);
        }
    }
    out
}

/// The most simultaneous connections this host can make to one destination, bounded by ephemeral
/// source ports (`net.ipv4.ip_local_port_range`) since one load window drives one destination.
/// Asking past it doesn't measure a bigger gateway: `connect` returns EADDRNOTAVAIL, which
/// `GenStats::rig_refused` must distinguish from the gateway refusing.
///
/// Returns the largest power of two fitting the host's range, since the ladder doubles and a
/// ceiling between rungs would make the top rung a different shape from the rest. Falls back to
/// Linux's documented default range when /proc can't be read (macOS, restricted containers).
/// TIME_WAIT is not fudged with a fraction; the orchestrator enables `tcp_tw_reuse` so the kernel
/// recycles ports itself, and this simply reads whatever range the host is actually configured with.
pub fn host_connection_ceiling() -> u32 {
    // Linux's compiled-in default, used only when the real one cannot be read.
    const STOCK_LINUX_RANGE: (u32, u32) = (32_768, 60_999);
    let (lo, hi) = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .ok()
        .and_then(|t| {
            let mut p = t.split_whitespace().filter_map(|v| v.parse::<u32>().ok());
            match (p.next(), p.next()) {
                (Some(lo), Some(hi)) if hi > lo => Some((lo, hi)),
                _ => None,
            }
        })
        .unwrap_or(STOCK_LINUX_RANGE);
    let usable = hi - lo + 1;
    let mut ceiling = 1u32;
    while ceiling * 2 <= usable {
        ceiling *= 2;
    }
    ceiling
}

/// Concurrency ceiling for a held-open stream, distinct from `host_connection_ceiling`'s request
/// bound: a stream holds its connection/fd for the whole window, so the binding resource is
/// descriptors, not ports (though on the bench box the raised fd limit means `min()` picks the port
/// term anyway). Uses a third of the descriptor budget, not clamped lower — measured field ceilings
/// exceed naive guesses, and an invented cap would clip real measurements. Very high concurrency is
/// actually bound by rig memory/CPU, a known gap this can't distinguish from gateway speed.
///
/// `STREAM_RUNAWAY_CAP` is a runaway backstop, not a measurement bound: it sits far above anything
/// plausible, so reaching it means investigate a bug, not "this is the gateway's ceiling".
const STREAM_RUNAWAY_CAP: u32 = 65_536;

pub fn stream_connection_ceiling() -> u32 {
    // Read the process's own limit rather than assuming a distro default: run-on-ec2.sh raises it,
    // and a derived ceiling means raising it is the only thing anyone has to change.
    let soft_fds = std::fs::read_to_string("/proc/self/limits")
        .ok()
        .and_then(|t| {
            t.lines()
                .find(|l| l.starts_with("Max open files"))
                .and_then(|l| {
                    l.split_whitespace()
                        .nth(3)
                        .and_then(|v| v.parse::<u32>().ok())
                })
        })
        // POSIX's own floor when the limit cannot be read. Deliberately small: guessing high here
        // would reintroduce exactly the ladder this function exists to bound.
        .unwrap_or(1024);
    stream_ceiling_from(soft_fds, host_connection_ceiling())
}

/// The derivation itself, PURE, so it can be tested against a box other than the one running the
/// test. The host-reading wrapper above cannot be: on a developer machine `/proc` is absent and both
/// ceilings collapse to their fallbacks, so a test driving it agrees with any implementation - the
/// exact shape of a guard that cannot fail. The numbers that mattered came from the bench box, and
/// this is what lets them be asserted from anywhere.
fn stream_ceiling_from(soft_fds: u32, port_ceiling: u32) -> u32 {
    let usable = (soft_fds / 3).max(1);
    let mut ceiling = 1u32;
    while ceiling * 2 <= usable {
        ceiling *= 2;
    }
    ceiling.min(port_ceiling).min(STREAM_RUNAWAY_CAP)
}

/// a real status with a healthy rig is the gateway's own answer, no HTTP answer at all is not.
pub fn probe_cell(cfg: &RunConfig, id: &CellId, mock_healthy: bool) -> Served {
    let (attempts, pause_s) = crate::probe::transient_budget();
    probe_cell_within(
        cfg,
        id,
        mock_healthy,
        attempts,
        Duration::from_secs(u64::from(pause_s)),
    )
}

/// The same probe over an explicit budget, so a test can exercise the retry loop without sleeping
/// through the field's real ~minute-long pause (which previously held sockets and starved parallel
/// tests). Mirrors the pattern `supervise.rs` already uses for the same reason.
pub fn probe_cell_within(
    cfg: &RunConfig,
    id: &CellId,
    mock_healthy: bool,
    attempts: u32,
    pause: Duration,
) -> Served {
    let (mut last, mut retryable) = probe_cell_once(cfg, id, mock_healthy);
    // Retry across the full `transient_budget()`: a single transient status (e.g. 503 from a
    // gateway briefly shedding load after the prior cell's heavy window) must not be recorded as a
    // permanent "does not serve" verdict. All cells get the same attempts/pause.
    for attempt in 1..attempts {
        let Some(why) = retryable.clone() else { break };
        eprintln!(
            "[probe] {id}: {why} - retry {attempt}/{} after {}s rather than recording a moment as \
             a capability",
            attempts - 1,
            pause.as_secs()
        );
        std::thread::sleep(pause);
        let (next, next_retryable) = probe_cell_once(cfg, id, mock_healthy);
        last = next;
        retryable = next_retryable;
    }
    last
}

/// One probe attempt: the verdict, and why it is worth asking again, if it is. Retryable answers
/// are transient statuses (503 and friends: "not right now") and transient transport failures (no
/// answer, or a refused connection: "not saying anything right now") — both are moments the
/// back-to-back, no-settle cell schedule can manufacture. A malformed response and an unknown
/// dialect are NOT retryable: the first is a real answer the gateway keeps giving, the second is
/// our own manifest and no amount of asking changes it.
fn probe_cell_once(cfg: &RunConfig, id: &CellId, mock_healthy: bool) -> (Served, Option<String>) {
    let Ok(ing) = id.ingress.parse::<Dialect>() else {
        return (
            Served::Untestable(format!("unknown ingress dialect {}", id.ingress)),
            None,
        );
    };
    let path = path_for(cfg, ing, &id.egress);
    let body = ing.body(&model_for(cfg, &id.egress));
    match http::post_json(
        cfg.gateway_addr,
        &path,
        body.as_bytes(),
        &headers_for(cfg, ing, &id.egress),
        cfg.probe_timeout,
    ) {
        Outcome::Response(r) if (200..300).contains(&r.status) => (Served::Yes, None),
        Outcome::Response(r) => {
            // Keep the actual status/body: a bare verdict can't distinguish "gateway declined" from
            // "rig-side reason produced 4xx on every cell".
            let evidence = crate::cell::Evidence {
                status: r.status,
                body_snippet: crate::cell::Evidence::snippet(&String::from_utf8_lossy(r.body())),
            };
            // A refusal we provoked is not a capability verdict: some dialects sign requests and the
            // harness sends a bearer token instead of forging a signature, so a gateway correctly
            // rejecting that with 401/403 must not be graded as a red. Decided here (needs the
            // dialect) rather than in `persistent_transient_verdict`, which stays a pure function of
            // the observed status.
            if ing.auth_is_unforgeable_by_the_rig() && matches!(r.status, 401 | 403) {
                return (Served::UnprobedAuth(evidence), None);
            }
            // NotConfigured: gateway says the pairing doesn't exist. Failed: gateway reached and
            // declined an otherwise-real pairing. NotVerified: rig couldn't get a fair reading, so
            // nothing was learned — must not be recorded as "does not serve".
            let retry = crate::probe::status_is_transient(r.status)
                .then(|| format!("HTTP {} is transient", r.status));
            match persistent_transient_verdict(Observation { status: Some(r.status), mock_healthy }) {
                v @ (Verdict::NotConfigured | Verdict::Failed) => {
                    // Only a Failed verdict is worth another ask: NotConfigured is the gateway
                    // stating the route does not exist, which asking again cannot change.
                    let retry = if v == Verdict::Failed { retry } else { None };
                    (Served::No(v, evidence), retry)
                }
                Verdict::NotVerified => (
                    Served::Untestable(format!(
                        "status {} observed, but the rig could not confirm itself, so this says nothing about the gateway",
                        r.status
                    )),
                    retry,
                ),
            }
        }
        // No HTTP answer: the gateway may never have been reached, so this is never a gateway fault.
        Outcome::ConnectionFailed(e) => (
            Served::Untestable(format!("no connection to the gateway: {e}")),
            Some("the connection was refused".to_string()),
        ),
        Outcome::TimedOut => (
            Served::Untestable("the gateway accepted the connection and never answered".into()),
            Some("the gateway did not answer in time".to_string()),
        ),
        // A response we cannot parse is a real answer the gateway keeps giving; retrying just
        // re-fetches the same bytes.
        Outcome::Malformed { message, .. } => (
            Served::Untestable(format!("unparseable response: {message}")),
            None,
        ),
        // We never asked: this describes a manifest defect of ours (a header we won't send), not
        // the gateway, so it's `Untestable` and never `Served::No`. No retry — the manifest says the
        // same thing every time. Logged loudly so the run points at the file to fix.
        Outcome::RigRefused(why) => {
            eprintln!(
                "probe: refused to send to {}>{} - {why}. The gateway was never asked; fix the \
                 manifest that declared it",
                id.ingress, id.egress
            );
            (
                Served::Untestable(format!(
                    "the rig refused to send this request, so the gateway was never asked: {why}"
                )),
                None,
            )
        }
    }
}

/// Drives the load generator at one concurrency, for the searches.
struct SweepProbe<'a> {
    cfg: &'a RunConfig,
    path: String,
    body: String,
    /// Same composed header list the probe authenticated this cell with, so the window and the
    /// probe can't end up speaking to the gateway as two different clients.
    headers: Vec<(String, String)>,
}

impl Probe for SweepProbe<'_> {
    fn probe(&mut self, concurrency: u32) -> Option<Sample> {
        // Load generator runs as its own pinned process (gateway 0-3, load 4-9, mock 10-15) so the
        // core split is the comparability basis of every published number; an unpinned generator
        // would measure a different machine.
        let stats = self.spawn_pinned(concurrency)?;
        // The OS refusing a thread means the window never ran at the requested concurrency: a rig
        // limit, not a gateway result, so the search must stop rather than read a turnover.
        if stats.spawn_failed {
            eprintln!("loadgen: could not reach c={concurrency}; the rig refused a thread");
            return None;
        }
        // Connections THIS HOST couldn't make (ephemeral ports/descriptors exhausted) are counted
        // separately from genuine gateway failures and treated as unmeasured, not a gateway result —
        // otherwise the search records our own port range as the gateway's ceiling.
        if stats.rig_refused > 0 {
            eprintln!(
                "loadgen: could not reach c={concurrency}; this host refused {} of its own connections \
                 (ephemeral ports or descriptors exhausted) - the window never ran at that concurrency",
                stats.rig_refused
            );
            return None;
        }
        // A window that produced nothing is unmeasured, not a zero.
        if stats.ok == 0 && stats.fail == 0 {
            return None;
        }
        // Carry the reading (p99/ok/fail) alongside the rate: the generator already measured it, so
        // one sweep can answer both throughput and latency-at-target from one set of windows on one
        // gateway state, instead of needing a second search after a restart.
        Some(
            Sample::new(stats.rps(), stats.fail == 0 && stats.ok > 0).with_reading(
                crate::search::Reading {
                    p99_us: stats.p99_us,
                    ok: stats.ok,
                    fail: stats.fail,
                },
            ),
        )
    }
}

impl SweepProbe<'_> {
    /// Run one window in a pinned child and read its stats line back.
    fn spawn_pinned(&self, concurrency: u32) -> Option<GenStats> {
        load_window(self.cfg, &self.path, &self.body, &self.headers, concurrency)
    }
}

/// Drive one pinned load window against the gateway and read the generator's stats line back.
/// Shared by the throughput search and the memory window so both load the box the same way (same
/// binary, pinning, process) — a memory number taken under different load isn't comparable.
/// Stop the gateway and start it again, returning only once it is ready to serve.
///
/// Needed so idle memory can be read from a process that hasn't served load; the prior approach of
/// reading RSS in place published post-load memory as idle. Errors carry the failed stage because
/// "could not restart" (gateway still up) and "restarted but never came back" (gateway down, every
/// later cell fails) are different findings.
pub fn restart_to_rest(
    spec: &crate::launch::LaunchSpec,
    launcher: &std::sync::Mutex<crate::launch::RealLauncher>,
    commands: &[String],
) -> Result<(), String> {
    let mut launcher = launcher
        .lock()
        .map_err(|_| "the launcher lock was poisoned".to_string())?;
    crate::supervise::stop_and_wait(&spec.runtime, spec.port, Duration::from_secs(30))
        .map_err(|e| format!("stopping it failed: {e:?}"))?;
    // The stop above already confirmed the previous native child is dead, so reaping it through the
    // same launcher that spawned it is a wait, never a hang.
    launcher.reap_previous_native_child();
    crate::launch::launch_default(&mut *launcher, spec)
        .map(|_| ())
        .map_err(|e| format!("it did not come back up: {e:?}"))?;
    // Replay post-boot commands: for docker the stop was `docker rm -f`, which destroys the
    // writable layer any admin-API config was written into. A failure here must propagate — a
    // half-configured gateway is worse than a down one, since later metrics would silently measure
    // the missing configuration.
    for line in commands {
        crate::launch::run_line(line, Duration::from_secs(120))
            .map_err(|why| format!("its post-boot configuration failed: {line}: {why}"))?;
    }
    Ok(())
}

pub fn load_window(
    cfg: &RunConfig,
    path: &str,
    body: &str,
    headers: &[(String, String)],
    concurrency: u32,
) -> Option<GenStats> {
    load_window_at(cfg, cfg.gateway_addr, path, body, headers, concurrency)
}

/// The same window, plus what the gateway spent serving it. Counters are read from the gateway's
/// process tree immediately before and after (not an absolute `utime`, which would carry startup
/// and every earlier window and make cell order look like a gateway property).
///
/// Cost is `Absent`, never zero, whenever it cannot be taken (pid unresolved, no /proc, or a
/// backwards counter from pid reuse) — an unmeasured gateway must never read as using no CPU.
/// Sampling itself is deliberately outside the window so its own cost cannot land inside it.
pub fn load_window_costed(
    cfg: &RunConfig,
    path: &str,
    body: &str,
    headers: &[(String, String)],
    concurrency: u32,
) -> (
    Option<GenStats>,
    crate::procsample::WindowCost,
    crate::measurement::Measurement<f64>,
) {
    // Utilisation is read from the same core declaration taskset was given; an empty spec (the
    // `smoke` path) yields no cores and an absent utilisation rather than measuring another
    // process's cores under this gateway's name.
    let started = std::time::Instant::now();
    let cores = crate::procsample::parse_cores(&cfg.gw_cores);
    let cpu_before = if cores.is_empty() {
        None
    } else {
        crate::procsample::cpu_busy_total(&crate::rss::RealProc, &cores)
    };
    let pid = crate::rss::root_pid(&cfg.runtime).copied();
    let before = match pid {
        Some(p) => crate::procsample::sample_live(p),
        None => crate::measurement::Measurement::absent_because(
            crate::measurement::Absent::NotMeasured,
            "the gateway's root pid did not resolve, so its cost cannot be attributed to this window",
        ),
    };
    let stats = load_window_at(cfg, cfg.gateway_addr, path, body, headers, concurrency);
    // Re-resolve rather than reuse `pid`: a restart between the two reads would otherwise subtract
    // a different process's sample as though it were the same one. Re-resolving turns a restart
    // into a backwards counter, which `procsample::cost` already refuses rather than publishing a
    // negative.
    let after = match crate::rss::root_pid(&cfg.runtime).copied() {
        Some(p) => crate::procsample::sample_live(p),
        None => crate::measurement::Measurement::absent_because(
            crate::measurement::Absent::NotMeasured,
            "the gateway's root pid did not resolve after the window",
        ),
    };
    // Requests come from the window's own completed count, never its published rate, which is
    // already derived — deriving cost from a derivation compounds the error.
    let requests = stats.as_ref().map(|s| s.ok).unwrap_or(0);
    let cost = crate::procsample::cost(&before, &after, requests, cfg.sweep_duration_s as f64);
    let cpu_after = if cores.is_empty() {
        None
    } else {
        crate::procsample::cpu_busy_total(&crate::rss::RealProc, &cores)
    };
    // Utilisation is derived from the gateway's own CPU accounting, not /proc/stat's tick-sampled
    // per-CPU counters: for bursty workloads (short requests completing between ticks) the tick
    // sample badly undercounts busy time (observed 14x-41x on tensorzero), and the error is worst
    // exactly where CPU-boundedness is most interesting, so it can't be corrected for uniformly.
    //
    // Wall time is measured here rather than taken from `sweep_duration_s`, the configured (not
    // actual) length.
    let elapsed_s = started.elapsed().as_secs_f64();
    let util = match (cores.is_empty(), cost.cpu_us.copied()) {
        (true, _) => crate::measurement::Measurement::absent_because(
            crate::measurement::Absent::NotMeasured,
            "this run declares no gateway core pinning, so there is no core set whose utilisation could be attributed to the gateway",
        ),
        (false, None) => crate::measurement::Measurement::absent_because(
            crate::measurement::Absent::NotMeasured,
            "the gateway CPU for this window is absent, so its share of the pinned cores cannot be derived",
        ),
        (false, Some(cpu_us)) if elapsed_s > 0.0 => crate::measurement::Measurement::Measured(
            (cpu_us / 1_000_000.0) / (elapsed_s * cores.len() as f64),
        ),
        (false, Some(_)) => crate::measurement::Measurement::absent_because(
            crate::measurement::Absent::NotMeasured,
            "the window reported no elapsed time, so utilisation cannot be divided out",
        ),
    };
    // Tick-sampled reading kept (not published) for deliberate comparison: agreement signals a
    // continuously-busy gateway, a large gap signals burstiness.
    let _tick_sampled = crate::procsample::utilisation(cpu_before, cpu_after);
    (stats, cost, util)
}

/// The same load window, driven at an explicit address rather than the gateway's. Needed so the
/// added-latency group's baseline leg can load the mock directly with the exact same generator,
/// pinning and windowing as the gateway-facing window — otherwise the gap between the two legs
/// would include rig noise, not purely what the gateway adds.
///
/// `headers` is required, not derived here: the child previously hardcoded a dummy bearer token,
/// so every window authenticated as a placeholder while the probe beside it used the real
/// per-dialect credential, silently failing 100% of windows for gateways with other auth. Taking
/// the composed header list from the same `headers_for` the probe uses prevents that recurring.
pub fn load_window_at(
    cfg: &RunConfig,
    addr: SocketAddr,
    path: &str,
    body: &str,
    headers: &[(String, String)],
    concurrency: u32,
) -> Option<GenStats> {
    {
        // A rig that cannot find its own binary empties every window of the run, so this must not
        // fail silently (same reasoning as the spawn failure below).
        let exe = match std::env::current_exe() {
            Ok(exe) => exe,
            Err(e) => {
                eprintln!(
                    "loadgen: could not resolve this binary's own path ({e}), so no load window can \
                     run at all - a rig fault, not the gateway's"
                );
                return None;
            }
        };
        let dur = cfg.sweep_duration_s.to_string();
        let conc = concurrency.to_string();
        let addr = addr.to_string();
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
            // Credential rides in the environment, not argv: argv is visible in `ps` to every user
            // on the box.
            .env(
                crate::loadgen::HEADERS_ENV,
                crate::loadgen::encode_headers(headers),
            )
            .stderr(std::process::Stdio::inherit())
            .output();
        // Must report the spawn/IO error rather than discard it (`.output().ok()?`): a missing
        // `taskset` or failed re-exec would otherwise silently empty every window of the entire run,
        // with the artifact blaming the gateway for a missing rig binary.
        let out = match out {
            Ok(out) => out,
            Err(e) => {
                eprintln!(
                    "loadgen: could not run the load generator ({e}) - this is a rig fault, not the \
                     gateway's, and it will empty every window of this run until it is fixed"
                );
                return None;
            }
        };
        let line = String::from_utf8_lossy(&out.stdout);
        let parsed = crate::loadgen::parse_ugen_line(line.trim());
        // A wire-contract violation (missing/non-numeric field in the stats line) still resolves to
        // `None` here, but the reason is logged to stderr first — otherwise it's indistinguishable
        // from an ordinary unmeasured window and the engine's own parsing bug looks like a gateway
        // or rig issue.
        if let (Some(reason), detail) = (parsed.reason(), parsed.detail()) {
            eprintln!(
                "loadgen: the stats line from our own child could not be read ({reason}){} - this \
                 window is unmeasured because of a harness fault, not because the gateway or the rig \
                 did anything",
                detail.map(|d| format!(": {d}")).unwrap_or_default()
            );
        }
        parsed.into_value().map(|u| GenStats {
            ok: u.ok.max(0) as u64,
            fail: u.fail.max(0) as u64,
            // `u.rps` is f64 (fractional below 1/s); a sub-1/s window would otherwise fail an i64
            // parse and be misclassified as a HarnessError.
            elapsed_s: if u.rps > 0.0 {
                u.ok as f64 / u.rps
            } else {
                0.0
            },
            latencies_us: Vec::new(),
            // Read from the child, not assumed `false` — otherwise `stats.spawn_failed` in
            // `SweepProbe::probe` could never fire on the subprocess path.
            spawn_failed: u.spawn_failed,
            rig_refused: u.rig_refused.max(0) as u64,
            budget_exceeded: u.budget_exceeded.max(0) as u64,
            // Subprocess sends back computed percentiles, not raw samples, so these come straight
            // from the stats line rather than being derived from the empty `latencies_us` above.
            p50_us: Some(u.p50_us.max(0) as u64),
            p99_us: Some(u.p99_us.max(0) as u64),
        })
    }
}

pub struct CellPerf {
    /// Every window the climb probed, in probe order — the whole of what this sweep produces.
    /// `frontier.rs` reads throughput at six declared tail-latency bounds directly off these rungs;
    /// there is no separate plateau/bisection summary anymore.
    pub points: Vec<crate::search::ProbedPoint>,
}

/// One load window at one concurrency — a point measurement, not a search. Asking a peak search for
/// a maximum over a range of one is a category error; a point measurement makes no turnover claim,
/// so there's nothing for a flanking check to refuse.
pub fn measure_at(cfg: &RunConfig, id: &CellId, concurrency: u32) -> Measurement<f64> {
    let Ok(ing) = id.ingress.parse::<Dialect>() else {
        return Measurement::absent_because(
            Absent::Untestable,
            format!("unknown ingress dialect {}", id.ingress),
        );
    };
    let mut p = SweepProbe {
        cfg,
        path: path_for(cfg, ing, &id.egress),
        body: ing.body(&model_for(cfg, &id.egress)),
        headers: headers_for(cfg, ing, &id.egress),
    };
    match p.probe(concurrency) {
        // A window with failures is not a throughput reading; the clean-window gate still applies.
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

/// Find the gateway's throughput peak on one served cell, and how much of it survives the 20ms
/// gate, from ONE sweep. Both readings must describe the same gateway state to be comparable; two
/// separate searches (one before, one after a cold restart) previously produced numbers up to 7%
/// apart purely from measuring different moments.
pub fn sweep_cell(cfg: &RunConfig, id: &CellId, lo: u32, hi: u32) -> CellPerf {
    let Ok(ing) = id.ingress.parse::<Dialect>() else {
        // The cell's own `served` verdict already explains why; no separate reason needed here.
        return CellPerf { points: Vec::new() };
    };
    let mut p = SweepProbe {
        cfg,
        path: path_for(cfg, ing, &id.egress),
        body: ing.body(&model_for(cfg, &id.egress)),
        headers: headers_for(cfg, ing, &id.egress),
    };
    // No start argument: the climb always begins at the floor. A start derived from the range made
    // a wider range open with a higher first probe.
    // No gate argument either: one stopping rule (stop when requests start failing), no summary —
    // every rung comes back and `frontier.rs` reads whichever bound a caller asks for.
    CellPerf {
        points: search::climb_rungs(&mut p, lo, hi),
    }
}

/// Ceiling for the sustained-throughput gate. README's own definition: "highest sustained
/// requests/sec with p99 under [a latency ceiling]". Named for the artifact field it feeds
/// (`rps_sustained_20ms`) so the constant and the number it produces cannot drift apart in a later
/// edit that changes one without the other.
pub const SUSTAINED_P99_CEILING_US: u64 = 20_000;

/// The error-rate half of the same gate. README: "...and a <0.1% error rate". Distinct from the
/// throughput sweep's all-or-nothing clean-window bar (`fail == 0`, see `SweepProbe`): a single
/// dropped connection here must not collapse "occasionally drops one in ten thousand" into the
/// same verdict as "cannot serve this concurrency at all".
pub const SUSTAINED_MAX_FAIL_RATIO: f64 = 0.001;

/// How many times the sustained ceiling may step down when it fails confirmation. Each step halves
/// the concurrency; bounds the walk to a few doublings below the bisection's answer.
/// Rungs at or below this drive too few sockets for draining to matter, and pausing after them is
/// pure schedule cost across a field sweep.
const STREAM_SETTLE_FREE_BELOW: u32 = 512;
/// One second of drain per 1,000 concurrent streams the last window drove.
const STREAM_SETTLE_MS_PER_1K: u64 = 1_000;
/// Backstop, sized to one TIME_WAIT generation (60s = longest a closed socket can need) rather than
/// a round number; the wait normally ends earlier on the observed counter. Past this the host is
/// broken, not draining — a finding, not a longer wait.
const STREAM_SETTLE_MAX_MS: u64 = 60_000;
/// How often the drain condition is re-read; 50ms keeps small-rung waits tight without making the
/// poll itself measurable.
const STREAM_SETTLE_POLL_MS: u64 = 50;
/// How far above this cell's own starting TIME_WAIT count still counts as drained. A bench box is
/// never perfectly idle, so this is a multiple of the cell's own starting count rather than a fixed
/// number, scaling to whatever that box considers quiet.
const STREAM_SETTLE_TW_TOLERANCE: u64 = 2;

thread_local! {
    /// This cell's starting TIME_WAIT count times the tolerance — the level `settle_after_streams`
    /// waits to come back down to. Recorded per cell, not once per process, since the box's quiet
    /// level drifts across a run's 36 back-to-back cells. `None` when /proc/net/sockstat is
    /// unavailable, so the caller falls back to the clock.
    static STREAM_SETTLE_TW_BASELINE: std::cell::RefCell<Option<u64>> =
        const { std::cell::RefCell::new(None) };
}

/// Record what "quiet" means for the cell about to be measured. Called once, before its ladder.
pub fn arm_stream_settle_baseline() {
    let tw = HostState::sample().tw.map(|t| t * STREAM_SETTLE_TW_TOLERANCE + 64);
    STREAM_SETTLE_TW_BASELINE.with(|b| *b.borrow_mut() = tw);
}
const MAX_CEILING_STEPDOWNS: usize = 4;

/// Why the sustained-stream search ended without a ceiling. Distinguishes rig-caused endings from
/// gateway-caused ones — publishing "the gateway did not hold the gate" for a window the rig failed
/// to take would be an invisible attribution error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamStop {
    /// The rig could not complete its windows at this concurrency. Never the gateway's result.
    RigRanShort { measured: usize, wanted: usize },
    /// Halving reached the floor of the searched range without finding a rung that holds.
    FloorReached { last: u32 },
    /// A stepped-down rung failed the gate on its very first window.
    SteppedRungFailed { at: u32 },
    /// The window could not be taken at all at this concurrency.
    WindowUnavailable { at: u32 },
    /// The step-down budget was genuinely spent without finding a rung that holds.
    BudgetExhausted,
    /// A rung failed at a concurrency this same cell had already proved clean. Never the gateway's
    /// result: a lower concurrency failing after a higher one passed means the rig hasn't drained.
    RigContaminated { at: u32, proven: u32 },
    /// The gateway stopped serving after overload and did not recover on its own. This one IS about
    /// the gateway — a real finding, published as the reason rather than an unexplained absence.
    ///
    /// `restart_cleared` distinguishes "wedged until restarted" from "wedged and stayed wedged".
    GatewayDidNotRecover {
        at: u32,
        proven: u32,
        restart_cleared: bool,
    },
}

impl StreamStop {
    /// May the search fall back to confirming the cell's own proven-clean floor? Only when the host
    /// itself isn't the suspect: `RigRanShort`/`WindowUnavailable`/`RigContaminated` all implicate
    /// the rig, so a fallback reading there would repeat the same attribution error with a
    /// flattering sign. `GatewayDidNotRecover` is excluded too — "it does not recover" is itself the
    /// finding and must not be overwritten by a number from a since-recovered process.
    fn floor_fallback_ok(self) -> bool {
        matches!(
            self,
            StreamStop::BudgetExhausted
                | StreamStop::FloorReached { .. }
                | StreamStop::SteppedRungFailed { .. }
        )
    }

    /// A rig failure is a `HarnessError`; everything else is a measurement that did not resolve.
    /// Filing our own shortfall under `NotMeasured` would put it among the gateway's results.
    fn absent_kind(self) -> Absent {
        match self {
            StreamStop::RigRanShort { .. }
            | StreamStop::WindowUnavailable { .. }
            | StreamStop::RigContaminated { .. } => Absent::HarnessError,
            // Gateway-not-recovering is the gateway's result, not ours.
            StreamStop::GatewayDidNotRecover { .. } => Absent::NotMeasured,
            _ => Absent::NotMeasured,
        }
    }

    fn describe(self, proved: u32, budget: usize) -> String {
        match self {
            StreamStop::RigRanShort { measured, wanted } => format!(
                "the bisection proved c={proved}, but re-measurement completed only {measured} of \
                 {wanted} windows - the RIG ran short, not the gateway, so no ceiling is published \
                 rather than one walked down on our own missing windows"
            ),
            StreamStop::FloorReached { last } => format!(
                "the bisection proved c={proved}, but it did not hold on re-measurement and halving \
                 reached the bottom of the searched range at c={last} without finding a concurrency \
                 that did"
            ),
            StreamStop::SteppedRungFailed { at } => format!(
                "the bisection proved c={proved}, but it did not hold on re-measurement and the \
                 stepped-down rung at c={at} failed the stream gate on its first window"
            ),
            StreamStop::WindowUnavailable { at } => format!(
                "the bisection proved c={proved}, but re-measurement could not take a window at all \
                 at c={at} - a rig failure, so nothing is published about the gateway here"
            ),
            StreamStop::RigContaminated { at, proven } => format!(
                "the bisection proved c={proved}, but re-measurement failed at c={at} - a concurrency \
                 THIS CELL had already carried cleanly up to c={proven}. A rung cannot fail below one \
                 it has already passed because of the gateway, so this is our rig not having drained \
                 between windows, and nothing is published about the gateway here"
            ),
            StreamStop::GatewayDidNotRecover { at, proven, restart_cleared } => format!(
                "the gateway stopped serving after being pushed past c={proven} and did not recover: \
                 c={at} failed repeatedly although this cell had already carried it cleanly{}. No \
                 sustained ceiling is published because a ceiling cannot be measured on a process \
                 that is no longer serving - but that it does not recover is itself the finding",
                if restart_cleared {
                    ", and it served again only after the harness restarted it"
                } else {
                    ", and a restart did not bring it back either"
                }
            ),
            StreamStop::BudgetExhausted => format!(
                "the bisection proved c={proved}, but that concurrency did not hold the stream gate \
                 on re-measurement and stepping down found none that did within {budget} attempts"
            ),
        }
    }
}

/// One rung as the sustained-throughput gate saw it, carrying the p99 and fail count behind its
/// pass/fail verdict. Distinct from `search::ProbedPoint`: the point is what the search made of a
/// window, this is what the gate made of it.
#[derive(Debug, Clone, PartialEq)]
pub struct SustainedPoint {
    pub concurrency: u32,
    pub passed: bool,
    pub rps: f64,
    pub p99_us: Option<u64>,
    /// Failed requests behind this rung, or `None` when no window reported. `Option`, not a bare
    /// `i64`: collapsing "no window" to 0 previously published a fabricated `fail: 0` reading a
    /// rung nothing was ever measured on.
    pub fail: Option<i64>,
}

/// Whether one window satisfies the sustained-throughput gate: p99 under the latency ceiling and
/// error rate under the README's 0.1% bar. A free function (not inlined into a probe) so the
/// pass/fail boundary is unit-testable without driving a real subprocess load window.
///
/// No longer called from the measurement path (the frontier replaced the scalar it gated — see
/// `record.rs`/`metric.rs`); kept only because it's the one executable statement of the README's
/// bar, which the frontier's `served_cleanly` still descends from.
pub fn sustained_gate_passes(p99_us: Option<u64>, ok: u64, fail: u64) -> bool {
    let total = ok + fail;
    let fail_ratio = if total == 0 {
        1.0
    } else {
        fail as f64 / total as f64
    };
    // No p99 reading counts as failing the latency half of the gate: the ceiling is a claim about
    // latency, and a rung with no latency reading has not earned it.
    let p99_ok = p99_us.is_some_and(|p| p < SUSTAINED_P99_CEILING_US);
    p99_ok && fail_ratio < SUSTAINED_MAX_FAIL_RATIO
}

/// How many content frames the MOCK sends per stream, read from the variable the mock reads.
///
/// Not `STREAM_FRAME_BUDGET` (how many frames the rig asks for) — this is how many the mock
/// actually sends, read from its own configuration rather than mirrored, since the ceiling below is
/// derived from it and a mirrored value could silently drift from the mock's real setting.
pub fn mock_stream_chunks() -> u32 {
    std::env::var("MOCK_STREAM_CHUNKS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        // Same reason `stream_pacing_interval_ms` rejects 0: the mock floors this at 1 (`chunks.max(1)`),
        // so a 0 here would describe a stream neither side produces.
        .filter(|v| *v > 0)
        .unwrap_or(64)
}

/// The most frames per second the mock can physically emit at this concurrency. Arithmetic, not a
/// measurement — we own the mock, so its ceiling is known rather than probed.
///
/// Replaces a measured direct-to-mock reference: the bench box's core partitioning makes that leg
/// structurally slower than the gateway leg (driving + reading both land on the loadgen's own cores
/// instead of being split across three core sets), so a measured reference systematically
/// understates the true ceiling and can wrongly flag a fast gateway as exceeding it.
///
/// Both terms are declared (`mock_stream_chunks` frames per stream, `stream_pacing_interval_ms`
/// between them, sleeping before every delta except the first), so nothing here is chosen.
pub fn mock_frame_ceiling_fps(concurrency: u32) -> f64 {
    let chunks = f64::from(mock_stream_chunks());
    let interval_s = stream_pacing_interval_ms() as f64 / 1000.0;
    let per_stream_s = (chunks - 1.0).max(1.0) * interval_s;
    if per_stream_s <= 0.0 || concurrency == 0 {
        return 0.0;
    }
    f64::from(concurrency) * chunks / per_stream_s
}

/// The mock's own delta pacing interval, read from the same env var the mock reads (default 20ms).
/// Together with `STREAM_STALL_MULTIPLIER` this defines "stalled" per the README ("no stream stalls
/// past 10x the mock's pacing interval"); both sides reading the same variable prevents the two from
/// silently diverging if the mock's pace is ever changed.
pub fn stream_pacing_interval_ms() -> u64 {
    std::env::var("MOCK_STREAM_INTERVAL_MS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        // Zero would drive the stall bound to 0, making every inter-frame gap count as a stall.
        .filter(|v| *v > 0)
        .unwrap_or(20)
}
/// Stall bound as a multiple of the mock's pacing interval: a gap past this means a stream went
/// quiet, not that it merely wobbled off the mock's clock. 10x rather than 2x, since 2x mostly
/// measured pacing fidelity under concurrency (the added_gap percentiles' job) and failed nearly
/// every gateway at every rung. Delivery (every frame arrives) and stalling (no dead air) are
/// deliberately separate concerns.
pub const STREAM_STALL_MULTIPLIER: u64 = 10;

/// Fraction of expected frames that must arrive, and the share of streams that may fail, for a
/// concurrency to hold the streams-sustained gate. Every frame, not 99.9%: a dropped frame is a
/// dropped token, and the sustained ceiling is meant to be the last rung before anything is lost.
pub const STREAM_MIN_DELIVERY_RATIO: f64 = 1.0;
pub const STREAM_MAX_ERROR_RATIO: f64 = 0.001;

/// What one window of `concurrency` concurrent streams did. Counts plus wall clock, all derived
/// from the same window, so "how many frames" and "how long" never come from two populations.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamWindow {
    pub concurrency: u32,
    /// Streams opened, and of those, the ones that never became a readable event stream at all. A
    /// stream that opened and delivered short is NOT an error, just a delivery shortfall — the two
    /// are separate halves of the README's gate.
    pub streams: u64,
    pub errored: u64,
    /// `errored`, split by attribution. Always sums to `errored`.
    pub error_kinds: StreamErrorKinds,
    /// Host socket/descriptor state sampled just before this window opened its connections.
    pub host_before: HostState,
    /// Every SSE event dispatched across every lane; what `fps` is computed from. May exceed
    /// `expected_frames`: each lane reads until its content budget is delivered
    /// (`http::SseBudget::Content`), so a gateway inserting pings or re-framing spends more events
    /// than the mock's layout implies. `expected_frames` stays the mock-shaped budget regardless.
    pub frames: u64,
    pub expected_frames: u64,
    /// Of `frames`, the ones carrying model output (`ingress::Dialect::sse_event_is_content`).
    /// Ledger RIG-11: the delivery gate must not count framing scaffolding (openai spends 3 events
    /// on framing, anthropic 5), so this pair is kept separate from `frames`/`expected_frames`,
    /// which legitimately want every event for fps / "did it stream at all".
    pub content_frames: u64,
    pub expected_content_frames: u64,
    /// Inter-frame gaps that exceeded the stall bound, summed across every lane.
    pub stalls: u64,
    pub elapsed_s: f64,
}

impl StreamWindow {
    /// Frames per second carried by the whole window. Zero elapsed is 0.0 rather than an infinity: a
    /// window with no measurable duration measured nothing, and an infinity would win every peak
    /// search it appeared in.
    pub fn fps(&self) -> f64 {
        if self.elapsed_s <= 0.0 {
            return 0.0;
        }
        self.frames as f64 / self.elapsed_s
    }

    /// Share of expected model output that arrived; numerator counts content frames, not every SSE
    /// event. Previously `frames / expected_frames`, which credited framing scaffolding the same as
    /// a token — a constant offset that differed between dialects on a value compared across them.
    pub fn delivery_ratio(&self) -> f64 {
        if self.expected_content_frames == 0 {
            return 0.0;
        }
        self.content_frames as f64 / self.expected_content_frames as f64
    }

    pub fn error_ratio(&self) -> f64 {
        if self.streams == 0 {
            return 1.0;
        }
        self.errored as f64 / self.streams as f64
    }

    /// Why this engine (not the gateway) is at fault for the numbers in this window, if at all.
    /// `None` means the counts are arithmetically possible, not that the gateway did well.
    ///
    /// A number that cannot happen is our bug, never the gateway's, so it's flagged `HarnessError`
    /// rather than published as a finding. Checked exactly, no tolerance, since both clauses are
    /// arithmetic over counts this rig took itself:
    /// - Content frames above the expected content budget (the mock's declared per-stream budget)
    /// - A non-finite or negative fps (broken clock or counter)
    ///
    /// Deliberately NOT checked: `frames` above `expected_frames`. That's legal — a gateway
    /// inserting pings or re-framing a translated stream spends more SSE events than the mock's
    /// layout implies, which is why the delivery gate counts content frames instead (Ledger RIG-11).
    pub fn engine_fault(&self) -> Option<String> {
        if self.content_frames > self.expected_content_frames {
            return Some(format!(
                "counted {} content frames where the mock's own budget for {} stream(s) is {} - a \
                 gateway cannot invent model output, so this rig counted wrong",
                self.content_frames, self.streams, self.expected_content_frames
            ));
        }
        let fps = self.fps();
        if !fps.is_finite() || fps < 0.0 {
            return Some(format!(
                "{} frames over {:.6}s yields {fps} frames/sec, which is not a rate - the window's \
                 clock or its counter is wrong",
                self.frames, self.elapsed_s
            ));
        }
        None
    }
}

/// Whether one window holds the README's streams-sustained gate: every expected content frame
/// arrived, no lane stalled past `STREAM_STALL_MULTIPLIER` times the mock's pace, and almost no
/// stream failed outright. A free function over plain counts, like `sustained_gate_passes`, so it's
/// unit-testable without a live mock.
pub fn streams_gate_passes(w: &StreamWindow) -> bool {
    streams_gate_verdict(w).is_none()
}

/// Why one window failed the gate, or `None` when it held. Names the tripped clause with its
/// counts so a failing rung publishes evidence, not a bare `passed: false`.
pub fn streams_gate_verdict(w: &StreamWindow) -> Option<String> {
    // A ratio computed from zero must never read as a clean window by floating-point accident.
    if w.streams == 0 {
        return Some("the window opened no stream, so it measured nothing".to_string());
    }
    if w.expected_content_frames == 0 {
        return Some(format!(
            "the window opened {} stream(s) but expected no content frames, so it measured nothing",
            w.streams
        ));
    }
    let mut why = Vec::new();
    if w.delivery_ratio() < STREAM_MIN_DELIVERY_RATIO {
        // Content frames on both sides of "of": the raw event count is larger and would otherwise
        // read as tokens missing that never carried a token in the first place.
        why.push(format!(
            "delivered {} of {} expected content frames ({} SSE events in total)",
            w.content_frames, w.expected_content_frames, w.frames
        ));
    }
    if w.stalls > 0 {
        why.push(format!(
            "{} inter-frame gap(s) past the {}ms stall bound",
            w.stalls,
            stall_bound_us() / 1000
        ));
    }
    if w.error_ratio() >= STREAM_MAX_ERROR_RATIO {
        why.push(format!("{} of {} streams errored", w.errored, w.streams));
    }
    if why.is_empty() {
        None
    } else {
        Some(why.join("; "))
    }
}

/// The stall bound in microseconds: `STREAM_STALL_MULTIPLIER` times the mock's own delta pacing.
fn stall_bound_us() -> u64 {
    stream_pacing_interval_ms() * STREAM_STALL_MULTIPLIER * 1_000
}

/// How many gaps in one lane's frame arrivals exceeded the stall bound. Gaps only, not the first
/// frame's offset (time-to-first-token is `Streaming`'s job) — otherwise a merely-slow-to-start
/// gateway would be charged as one going quiet mid-stream.
fn stalls_in(offsets: &[u64]) -> u64 {
    offsets
        .windows(2)
        .filter(|w| w[1].saturating_sub(w[0]) > stall_bound_us())
        .count() as u64
}

/// Whether one lane's outcome is a stream that never existed, as opposed to one that ran short.
/// Zero frames is an error, not a shortfall (README: a 200 that buffers and never frames is "did
/// not stream"), so it must not be folded into the delivery ratio — that would average away
/// gateways that streamed nothing entirely at high concurrency.
/// Host socket/descriptor state sampled immediately before a window opens its connections, used to
/// tell "gateway changed its mind" from "host hadn't recovered" when a rung fails after a lower one
/// passed:
///
///   * `tw` - sockets in TIME_WAIT (`/proc/net/sockstat`); the residue that takes longest to clear.
///   * `tcp_inuse` / `tcp_alloc` - live/allocated TCP sockets; a leak shows as a floor that persists.
///   * `fds` - open descriptors in this process (`/proc/self/fd`).
///
/// All absent on a non-Linux host, which is honest rather than zero: a zero here would read as
/// "the box was clean" on a machine that simply cannot answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HostState {
    pub tw: Option<u64>,
    pub tcp_inuse: Option<u64>,
    pub tcp_alloc: Option<u64>,
    pub fds: Option<u64>,
}

impl HostState {
    pub fn sample() -> Self {
        let sockstat = std::fs::read_to_string("/proc/net/sockstat").unwrap_or_default();
        // "TCP: inuse 5 orphan 0 tw 12 alloc 20 mem 2" - read by NAME, never by position: the field
        // set differs across kernels and a positional read would silently return someone else's
        // number rather than nothing.
        let field = |name: &str| -> Option<u64> {
            let line = sockstat.lines().find(|l| l.starts_with("TCP:"))?;
            let mut it = line.split_whitespace();
            while let Some(tok) = it.next() {
                if tok == name {
                    return it.next().and_then(|v| v.parse().ok());
                }
            }
            None
        };
        Self {
            tw: field("tw"),
            tcp_inuse: field("inuse"),
            tcp_alloc: field("alloc"),
            fds: std::fs::read_dir("/proc/self/fd")
                .ok()
                .map(|d| d.count() as u64),
        }
    }
    fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "tw": self.tw, "tcp_inuse": self.tcp_inuse,
            "tcp_alloc": self.tcp_alloc, "fds": self.fds,
        })
    }
}

/// Why a stream errored, not just that it did — a single `errored` count can't distinguish "us"
/// from "them". Attribution classes, not error codes: connect-failed (peer declined/reset), status
/// (peer answered no), no-frames (2xx, no event), malformed/not-event-stream (not SSE). Rig-side
/// ends never reach here — `stream_window` discards those windows entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StreamErrorKinds {
    /// The peer refused, reset, or was unreachable - the connection never carried a request.
    pub connect_failed: u64,
    /// The peer answered with a non-2xx status.
    pub status: u64,
    /// A 2xx that delivered no event at all.
    pub no_frames: u64,
    /// The peer answered 2xx but did not speak a well-formed event stream.
    pub not_event_stream: u64,
}

impl StreamErrorKinds {
    pub fn total(&self) -> u64 {
        self.connect_failed + self.status + self.no_frames + self.not_event_stream
    }
    fn add(&mut self, o: &crate::http::SseOutcome) {
        use crate::http::SseEnd;
        // Order matters and mirrors `stream_errored`: testing frames before status would file a
        // refused connection as "no frames" instead of "connect failed".
        if matches!(o.end, SseEnd::ConnectionFailed(_)) {
            self.connect_failed += 1;
        } else if !o.status.is_some_and(|s| (200..300).contains(&s)) {
            self.status += 1;
        } else if matches!(o.end, SseEnd::Malformed(_) | SseEnd::NotAnEventStream(_)) {
            self.not_event_stream += 1;
        } else {
            self.no_frames += 1;
        }
    }
}

fn stream_errored(o: &crate::http::SseOutcome) -> bool {
    // Rig running out of ports is not an errored stream: the gateway was never asked. `stream_window`
    // discards the whole window instead — see `rig_exhausted_in`.
    if matches!(o.end, crate::http::SseEnd::RigExhausted(_)) {
        return false;
    }
    // Neither is a request we refused to send; the gateway was never asked either. Discarded the
    // same way, loudly, by `stream_window`.
    if matches!(o.end, crate::http::SseEnd::RigRefused(_)) {
        return false;
    }
    if !o.status.is_some_and(|s| (200..300).contains(&s)) {
        return true;
    }
    if o.frame_offsets_us.is_empty() {
        return true;
    }
    // `EventCeilingReached` deliberately not included: a lane that exhausted the event ceiling
    // without delivering its content budget is a delivery shortfall (shows in the ratio), not a
    // structural stream error — counting both would double-count one failure.
    matches!(
        o.end,
        crate::http::SseEnd::ConnectionFailed(_)
            | crate::http::SseEnd::Malformed(_)
            | crate::http::SseEnd::NotAnEventStream(_)
    )
}

/// Drive `concurrency` concurrent streams against `addr` and read each one to the frame budget.
///
/// `None` means the window never ran at the requested concurrency (OS refused a lane) — a rig
/// limit, so the search must stop rather than read the shortfall as a turnover.
///
/// In-process tasks, not `load_window`'s pinned child: a stream lane sleeps between the mock's 20ms
/// deltas rather than saturating a core, and the mock-ceiling reference is taken with this same
/// function, so instrument overhead is charged identically to both legs. Trades away the generator's
/// core pinning, which matters only at concurrencies high enough to saturate the box — hence the
/// mock-bound guardrail applied to these numbers rather than publishing them bare.
///
/// `dialect` is required: without it the delivery ratio counts protocol scaffolding as delivered
/// tokens (ledger RIG-11).
/// Let the host drain between stream windows, scaled to what the last one drove. A window at high
/// concurrency leaves that many sockets in FIN_WAIT/TIME_WAIT; measuring the next rung immediately
/// after would measure residue as much as the gateway (confirmed: bisection failures where a lower
/// concurrency failed right after a higher one had passed cleanly).
///
/// Waits for the host to report drained (via TIME_WAIT count, `HostState`) rather than a fixed
/// sleep, since no constant duration is right for every gateway/rung — a fixed sleep was measured to
/// be both too long (tens of minutes of pure waiting across a field run) and too short (residue still
/// present after the old cap). The 60s backstop is one TIME_WAIT generation: the longest a closed
/// socket can physically need. Past it the host is broken, not draining — a finding, not a longer wait.
fn settle_after_streams(concurrency: u32) {
    if concurrency <= STREAM_SETTLE_FREE_BELOW {
        return;
    }
    let baseline = STREAM_SETTLE_TW_BASELINE.with(|b| *b.borrow());
    let Some(target) = baseline else {
        // No baseline means no /proc to read; fall back to the proportional sleep.
        let ms = u64::from(concurrency) * STREAM_SETTLE_MS_PER_1K / 1000;
        std::thread::sleep(std::time::Duration::from_millis(ms.min(STREAM_SETTLE_MAX_MS)));
        return;
    };
    let started = std::time::Instant::now();
    let deadline = std::time::Duration::from_millis(STREAM_SETTLE_MAX_MS);
    loop {
        let now = HostState::sample();
        match now.tw {
            // Drained: TIME_WAIT is back within tolerance of where this cell started.
            Some(tw) if tw <= target => return,
            // Counter stopped being readable mid-sweep; fall back to the clock rather than waiting
            // on a signal that no longer exists.
            None => {
                std::thread::sleep(std::time::Duration::from_millis(
                    (u64::from(concurrency) * STREAM_SETTLE_MS_PER_1K / 1000)
                        .min(STREAM_SETTLE_MAX_MS),
                ));
                return;
            }
            Some(_) => {}
        }
        if started.elapsed() >= deadline {
            eprintln!(
                "streams: the host has not drained to tw<={target} in {}s after c={concurrency} \
                 (tw={:?}) - measuring anyway, and the rung that follows carries that residue",
                deadline.as_secs(),
                now.tw
            );
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(STREAM_SETTLE_POLL_MS));
    }
}

pub fn stream_window(
    addr: SocketAddr,
    path: &str,
    body: &str,
    headers: &[(String, String)],
    dialect: Dialect,
    concurrency: u32,
) -> Option<StreamWindow> {
    let budget = crate::metric::STREAM_FRAME_BUDGET;
    // The most content frames a lane's budget can hold: the budget minus what this dialect spends
    // before its first token. See `Dialect::stream_prelude_frames`.
    let content_budget = (budget as u64).saturating_sub(dialect.stream_prelude_frames());
    // Read is budgeted in content frames, not events, so the ratio's denominator is safe to compare
    // against: reading to a fixed event count would let any non-content event a gateway adds (pings,
    // re-framing, keepalives) displace a content frame and depress delivery on gateways that lost
    // nothing — same class of bug `STREAM_STALL_MULTIPLIER` fixed on the stall clause. Bounded by
    // `STREAM_EVENT_CEILING`/`STREAM_TIMEOUT`; a lane that hits the ceiling short of its content is
    // still a real shortfall.
    let lane_budget = crate::http::SseBudget::Content {
        frames: content_budget,
        event_ceiling: crate::metric::STREAM_EVENT_CEILING,
    };

    // One tokio task per lane, not one OS thread: a thread per lane caps far below the throughput
    // searches (scheduler thrashing at tens of thousands of threads, not a bigger gateway). The lane
    // body (`post_json_sse_async`) feeds the same `SseReader` and sends the same bytes as the
    // blocking lane (differential-tested), so this changes only who owns the waiting.
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("stream window: could not build a runtime for c={concurrency}: {e}");
            return None;
        }
    };
    let path = path.to_string();
    let body = body.to_string();
    let headers = headers.to_vec();

    // Sampled before any connection opens, so it reflects what the window inherited, not what it
    // leaves behind.
    let host_before = HostState::sample();

    let (outcomes, panicked, elapsed_s): (Vec<crate::http::SseOutcome>, usize, f64) =
        rt.block_on(async move {
            let mut lanes = Vec::with_capacity(concurrency as usize);
            for _ in 0..concurrency {
                let path = path.clone();
                let body = body.clone();
                let headers = headers.clone();
                lanes.push(tokio::spawn(async move {
                    crate::http::post_json_sse_async(
                        addr,
                        &path,
                        body.as_bytes(),
                        &headers,
                        crate::metric::STREAM_TIMEOUT,
                        lane_budget,
                        Some(dialect),
                    )
                    .await
                }));
            }
            // Clock starts once every lane exists (mirrors `gen.rs::run`): the ramp of creating them
            // must not land in the denominator, or fps() is depressed hardest at the high rungs.
            let started = std::time::Instant::now();
            let mut out = Vec::with_capacity(lanes.len());
            let mut panicked = 0usize;
            for l in lanes {
                // A panicked lane is a harness fault, not a gateway failure — kept out of the
                // gateway's error rate but still counted (via `panicked`), so it isn't silently lost.
                match l.await {
                    Ok(o) => out.push(o),
                    Err(_) => panicked += 1,
                }
            }
            (out, panicked, started.elapsed().as_secs_f64())
        });
    let mut w = StreamWindow {
        concurrency,
        streams: 0,
        errored: 0,
        error_kinds: StreamErrorKinds::default(),
        host_before,
        frames: 0,
        expected_frames: 0,
        content_frames: 0,
        expected_content_frames: 0,
        stalls: 0,
        elapsed_s,
    };
    // A request we refused to send measured nothing at all — a manifest defect, not a resource
    // limit. Loud, unmeasured, never the gateway's: see `http::SseEnd::RigRefused`.
    if let Some(why) = outcomes.iter().find_map(|o| match &o.end {
        crate::http::SseEnd::RigRefused(why) => Some(why.clone()),
        _ => None,
    }) {
        eprintln!(
            "stream window: refused to send at c={concurrency} - {why}. The gateway was never asked, \
             so this window measured nothing about it; fix the manifest that declared it"
        );
        return None;
    }
    // A window that ran out of rig capacity never ran at the concurrency it claims. Unmeasured, not
    // a failing rung — the alternative publishes this host's port range as the gateway's ceiling.
    let rig_exhausted = outcomes
        .iter()
        .filter(|o| matches!(o.end, crate::http::SseEnd::RigExhausted(_)))
        .count();
    if rig_exhausted > 0 {
        eprintln!(
            "stream window: could not reach c={concurrency}; this host refused {rig_exhausted} of its \
             own connections (ephemeral ports or descriptors exhausted) - the window never ran at that concurrency"
        );
        return None;
    }
    for o in outcomes {
        w.streams += 1;
        w.expected_frames += budget as u64;
        w.expected_content_frames += content_budget;
        if stream_errored(&o) {
            w.errored += 1;
            w.error_kinds.add(&o);
        }
        w.frames += o.frame_offsets_us.len() as u64;
        w.content_frames += o.content_frames;
        w.stalls += stalls_in(&o.frame_offsets_us);
    }
    // A WINDOW MISSING LANES NEVER RAN AT THE CONCURRENCY IT CLAIMS. Identical reasoning to the
    // rig-exhaustion check above, which already refuses a window for the same defect arriving by a
    // different route - and refusing it is not the conservative choice here, it is the correct one.
    //
    // Both `streams` and `expected_frames` are accumulated PER SURVIVING LANE, so a panicked lane
    // left the numerator and the denominator alike: a window where half the lanes died reported the
    // surviving half's delivery ratio as though it were the whole window's, and passed the gate on
    // it. The ratio looked perfect precisely because the evidence of the failure had been removed
    // from both sides of it. That is a flattering number, published as a measurement.
    //
    // LOUD, because the silence was the real defect. This was the ONLY one of this function's four
    // refusals that returned without saying anything, and it is the one that fired: busbar's cpu_fps
    // took 0.0s on every streamable cell across a four-hour run and left not one line to explain it.
    if let Some(why) = window_refusal(panicked, w.streams, concurrency) {
        eprintln!("{why}");
        return None;
    }
    // AND A WINDOW WHOSE OWN COUNTS CANNOT HAPPEN IS DISCARDED, for the same reason a panicked lane is:
    // it is a fault of ours, and publishing it would attribute our defect to the gateway. Loud and
    // named, because a silently dropped impossible window is how a counting bug survives a four-hour
    // run. See `StreamWindow::engine_fault` - the check is exact arithmetic over counts this rig took
    // itself, so it cannot misfire on a gateway that is merely fast or merely unusual.
    if let Some(why) = w.engine_fault() {
        eprintln!("stream window: c={concurrency} is an ENGINE FAULT, not a measurement - {why}");
        return None;
    }
    Some(w)
}

/// May a stream ceiling be published from these windows? Pure, so testable without a socket.
///
/// Two conditions required (minimum window count AND majority): checking only the majority lets a
/// window that couldn't run skip without incrementing `total`, so two absent repeats could leave
/// 1-of-1 published as a confirmed ceiling — a single-lucky-window inversion class C6 catches
/// downstream (`confirm_ceiling` refuses the same input on the throughput side).
fn stream_ceiling_confirmed(total: usize, held: usize) -> bool {
    total >= crate::search::WINDOWS_PER_RUNG && held * 2 > total
}

/// Why a stream window must be discarded rather than published, or `None` when it may stand. Kept
/// pure (separate from the window that feeds it) so it's unit-testable.
fn window_refusal(panicked: usize, streams: u64, concurrency: u32) -> Option<String> {
    if panicked > 0 {
        return Some(format!(
            "stream window: {panicked} of {concurrency} lanes PANICKED - a harness fault, not the \
             gateway's. The window is discarded rather than published from the survivors, because \
             their delivery ratio would count neither the failures nor what they were expected to \
             deliver"
        ));
    }
    if streams == 0 {
        return Some(format!(
            "stream window: c={concurrency} produced no streams at all - unmeasured, not a failing \
             window"
        ));
    }
    None
}

/// One rung a stream search actually probed, carrying the counts behind its verdict. Its own type
/// rather than `search::ProbedPoint` since it needs delivery counts the generic point doesn't carry.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamPoint {
    pub concurrency: u32,
    pub passed: bool,
    pub fps: f64,
    pub frames: u64,
    pub expected_frames: u64,
    /// The two counts the delivery clause is actually computed from (not `frames`/`expected_frames`,
    /// which count every SSE event and can legitimately exceed the content pair on a gateway that
    /// inserts framing).
    pub content_frames: u64,
    pub expected_content_frames: u64,
    pub streams: u64,
    pub errored: u64,
    /// `errored`, split by attribution — see `StreamErrorKinds`. Carried on the point (not just the
    /// window) because the sweep array is what survives into the snapshot.
    pub error_kinds: StreamErrorKinds,
    /// Host state this rung was measured against — see `HostState`. On the point for the same reason.
    pub host_before: HostState,
    pub stalls: u64,
    /// Why the gate failed, from `streams_gate_verdict`, when it did.
    pub why: Option<String>,
}

impl StreamPoint {
    /// The published rung, self-describing: every count the verdict was computed from, so a reader
    /// can re-derive pass/fail rather than trust it. Decides the wire shape once, here, rather than
    /// at each of the two searches that emit it as `Vec<serde_json::Value>`.
    pub fn to_json(&self) -> serde_json::Value {
        let mut v = serde_json::json!({
            "conc": self.concurrency,
            "passed": self.passed,
            "fps": self.fps,
            "frames": self.frames,
            "frames_expected": self.expected_frames,
            "content_frames": self.content_frames,
            "content_frames_expected": self.expected_content_frames,
            "streams": self.streams,
            "stream_errors": self.errored,
            // Always emitted, zeros included, so a clean rung and an unrecorded one are
            // distinguishable in the snapshot.
            "stream_errors_connect_failed": self.error_kinds.connect_failed,
            "stream_errors_status": self.error_kinds.status,
            "stream_errors_no_frames": self.error_kinds.no_frames,
            "stream_errors_not_event_stream": self.error_kinds.not_event_stream,
            "host_before": self.host_before.to_json(),
            "stalls": self.stalls,
        });
        if let (Some(why), Some(obj)) = (&self.why, v.as_object_mut()) {
            obj.insert("why".to_string(), serde_json::json!(why));
        }
        v
    }
}

fn point_of(w: &StreamWindow, passed: bool) -> StreamPoint {
    StreamPoint {
        concurrency: w.concurrency,
        passed,
        fps: w.fps(),
        frames: w.frames,
        expected_frames: w.expected_frames,
        content_frames: w.content_frames,
        expected_content_frames: w.expected_content_frames,
        streams: w.streams,
        errored: w.errored,
        error_kinds: w.error_kinds,
        host_before: w.host_before,
        stalls: w.stalls,
        why: if passed {
            None
        } else {
            streams_gate_verdict(w)
        },
    }
}

/// Drives concurrent streams at one concurrency for the streams-sustained GATE.
struct StreamGateProbe {
    addr: SocketAddr,
    path: String,
    body: String,
    headers: Vec<(String, String)>,
    dialect: Dialect,
    points: Vec<StreamPoint>,
}

impl Probe for StreamGateProbe {
    fn probe(&mut self, concurrency: u32) -> Option<Sample> {
        let w = stream_window(
            self.addr,
            &self.path,
            &self.body,
            &self.headers,
            self.dialect,
            concurrency,
        )?;
        let passed = streams_gate_passes(&w);
        self.points.push(point_of(&w, passed));
        Some(Sample::new(w.fps(), passed))
    }
}

/// What a stream search found on one cell.
pub struct CellStreams {
    /// The winning concurrency (a gate ceiling, or the concurrency a peak happened at).
    pub concurrency: Measurement<u32>,
    pub fps: Measurement<f64>,
    pub points: Vec<StreamPoint>,
}

/// Where a stream search drives, resolved ONCE: a cell whose path, streaming body and headers were
/// each worked out separately by the two searches could have them drift apart, and the two numbers
/// would then be about two different wires.
struct StreamTarget {
    path: String,
    body: String,
    headers: Vec<(String, String)>,
    /// The ingress wire these frames come back in. Resolved here with everything else for the same
    /// reason: the two searches must classify content frames by the SAME dialect they addressed the
    /// gateway in, or their delivery ratios are about two different protocols.
    dialect: Dialect,
}

fn stream_target(cfg: &RunConfig, id: &CellId) -> Option<StreamTarget> {
    let ing = id.ingress.parse::<Dialect>().ok()?;
    Some(StreamTarget {
        path: path_for(cfg, ing, &id.egress),
        body: ing.stream_body(&model_for(cfg, &id.egress)),
        headers: headers_for(cfg, ing, &id.egress),
        dialect: ing,
    })
}

/// Find the highest concurrency at which the gateway still carries clean streams.
///
/// `bisect_ceiling`, not `saturation_plateau`: this is a monotone pass/fail gate in concurrency,
/// exactly like `sweep_sustained_cell`. Once frames start arriving late or short, adding more
/// concurrency does not bring them back.
pub fn sweep_streams_cell(cfg: &RunConfig, id: &CellId, lo: u32, hi: u32) -> CellStreams {
    // What "drained" means for this cell, recorded before driving anything and re-armed per cell
    // (the box's quiet level drifts across a run's 36 back-to-back cells).
    arm_stream_settle_baseline();
    let Some(t) = stream_target(cfg, id) else {
        return CellStreams {
            concurrency: Measurement::absent(Absent::Untestable),
            fps: Measurement::absent(Absent::Untestable),
            points: Vec::new(),
        };
    };
    let mut p = StreamGateProbe {
        addr: cfg.gateway_addr,
        path: t.path,
        body: t.body,
        headers: t.headers,
        dialect: t.dialect,
        points: Vec::new(),
    };
    // The stream ceiling, not the caller's request ceiling: see `stream_connection_ceiling`.
    let hi = hi.min(stream_connection_ceiling());
    let lo = lo.min(hi);
    let r = search::bisect_ceiling(&mut p, lo, hi);
    match r.ceiling.copied() {
        // `bisect_ceiling`'s own measured "nothing sustains this gate": a real zero, not a missed
        // lookup.
        Some(0) => CellStreams {
            concurrency: Measurement::Measured(0),
            fps: Measurement::Measured(0.0),
            points: p.points,
        },
        // Needs confirmation: `bisect_ceiling` lands exactly on the boundary rung (highest
        // concurrency that passed once), which can be marginal — re-measurement found some
        // "sustained" ceilings held in only 1 of 3 windows.
        Some(c) => match p.points.iter().find(|pt| pt.concurrency == c).map(|pt| pt.fps) {
            Some(v) => {
                // Top of this cell's own uncontaminated ascending prefix (every rung up to and
                // including it passed before anything failed) — the only concurrency here certainly
                // not an artefact of a busy host. Used below as the line under which a failure
                // cannot honestly be the gateway's.
                let proven_clean = p
                    .points
                    .iter()
                    .take_while(|pt| pt.passed)
                    .map(|pt| pt.concurrency)
                    .max()
                    .unwrap_or(0);
                let mut ceiling = c;
                let mut first_fps = v;
                let mut winner: Option<(u32, f64)> = None;
                // Defaults to the budget case, which is what the loop ends on when nothing else
                // interrupts it; every other exit overwrites this at its own `break`.
                let mut stop = StreamStop::BudgetExhausted;
                // A stepped rung whose seed window fails must step down again, not end the search:
                // ending here previously left concurrencies untried and most of the step-down budget
                // unspent, publishing absence where a number was likely available. The failed seed
                // must not get confirmation windows either — a rung that couldn't seed hasn't earned
                // a vote, so it's simply abandoned and the search continues.
                let mut seed_failed = false;
                for _ in 0..MAX_CEILING_STEPDOWNS {
                    if seed_failed {
                        // Abandon unconfirmed; fall to another step-down rather than voting on a
                        // window that never held.
                        seed_failed = false;
                    } else {
                    let mut held = 1usize; // the bisection's own winning window is a real vote
                    let mut total = 1usize;
                    let mut rates = vec![first_fps];
                    for _ in 1..crate::search::WINDOWS_PER_RUNG {
                        settle_after_streams(ceiling);
                        let Some(w) = stream_window(cfg.gateway_addr, &p.path, &p.body, &p.headers, p.dialect, ceiling) else {
                            continue;
                        };
                        let passed = streams_gate_passes(&w);
                        p.points.push(point_of(&w, passed));
                        total += 1;
                        if passed {
                            held += 1;
                            rates.push(w.fps());
                        }
                    }
                    // Same two-part rule `confirm_ceiling` uses (majority AND enough windows), not
                    // just the majority half: without the minimum-window check, two absent repeats
                    // could leave a single unrepeated window published as a confirmed ceiling.
                    if stream_ceiling_confirmed(total, held) {
                        // Published rate is the median of the windows that held, matching every
                        // other repeated measurement in this engine.
                        rates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                        let fps = crate::search::nearest_rank_median(&rates).unwrap_or(first_fps);
                        winner = Some((ceiling, fps));
                        break;
                    }
                    if total < crate::search::WINDOWS_PER_RUNG {
                        // (see `stream_ceiling_confirmed`)
                        // THE RIG RAN SHORT, NOT THE GATEWAY. Stepping down would walk the ceiling
                        // down on our own missing windows and charge the gateway for them, so this
                        // ends the search with no winner and the cell publishes an absence carrying
                        // the reason - the same choice `confirm_ceiling` makes.
                        eprintln!(
                            "streams: c={ceiling} could only be measured in {total} of {} windows - \
                             the rig ran short, so no ceiling is published rather than one taken from \
                             a single window",
                            crate::search::WINDOWS_PER_RUNG
                        );
                        stop = StreamStop::RigRanShort {
                            measured: total,
                            wanted: crate::search::WINDOWS_PER_RUNG,
                        };
                        break;
                    }
                    }
                    // Step down inside the bracket, not by halving: halving would discard everything
                    // the ascending sweep and bisection already established, potentially dropping
                    // below a concurrency already proven clean. Bisect between the known-good floor
                    // (top of this cell's uncontaminated ascending prefix) and the failed rung;
                    // halving is only the fallback when there's no bracket to bisect within.
                    let known_good = if proven_clean > 0 { proven_clean.max(lo) } else { lo };
                    let next = if known_good < ceiling {
                        let mid = known_good + (ceiling - known_good) / 2;
                        // A bracket one rung wide bisects to its own floor; stepping to `ceiling`
                        // would loop forever, so the floor is the honest next probe.
                        if mid >= ceiling { known_good } else { mid }
                    } else {
                        ceiling / 2
                    };
                    if next < lo.max(1) || next == ceiling {
                        stop = StreamStop::FloorReached { last: ceiling };
                        break;
                    }
                    eprintln!("streams: c={ceiling} did not hold - stepping down to c={next}");
                    ceiling = next;
                    // We just drove `ceiling * 2` streams through this host; the stepped rung is the
                    // measurement most exposed to that residue, because it is the one that decides
                    // whether the search ends with a number or with nothing.
                    settle_after_streams(next * 2);
                    // The stepped rung's own first window seeds the next iteration's `held = 1`,
                    // so it must actually HOLD the gate to be that vote - `confirm_ceiling` has the
                    // same rule. Seeding with a failing window let a rung reach majority with only
                    // one real hold out of three, folding a gate-failing window's rate into the
                    // published sustained figure.
                    match stream_window(cfg.gateway_addr, &p.path, &p.body, &p.headers, p.dialect, ceiling) {
                        Some(w) => {
                            let passed = streams_gate_passes(&w);
                            p.points.push(point_of(&w, passed));
                            if !passed {
                                // A rung cannot fail below one it has already passed — that points at
                                // an undrained rig, not the gateway. Retry once after a full settle
                                // (cheap, and the likeliest explanation); gated on the same
                                // `STREAM_SETTLE_FREE_BELOW` threshold the settle itself uses, so this
                                // can't become a blanket excuse for real gateway failures at low
                                // concurrency where there's no drain residue to speak of.
                                let contaminated =
                                    proven_clean > STREAM_SETTLE_FREE_BELOW && ceiling <= proven_clean;
                                let recovered = if contaminated {
                                    eprintln!(
                                        "streams: c={ceiling} failed the gate although this cell already                                          carried c={proven_clean} cleanly - settling and re-taking the window"
                                    );
                                    std::thread::sleep(std::time::Duration::from_millis(STREAM_SETTLE_MAX_MS));
                                    match stream_window(cfg.gateway_addr, &p.path, &p.body, &p.headers, p.dialect, ceiling) {
                                        Some(w2) => {
                                            let passed2 = streams_gate_passes(&w2);
                                            p.points.push(point_of(&w2, passed2));
                                            if passed2 { first_fps = w2.fps(); }
                                            passed2
                                        }
                                        None => false,
                                    }
                                } else {
                                    false
                                };
                                // A settle didn't help, so a restart is the experiment that separates
                                // "rig still dirty" from "gateway stopped serving and isn't coming
                                // back". No number is published on the far side either way — a
                                // restarted process's reading isn't a fact about the one that served
                                // the prior rungs; the finding itself ("did not recover") is published
                                // instead.
                                if !recovered && contaminated {
                                    let restart_cleared = match cfg.relaunch.as_ref() {
                                        Some(spec) => {
                                            eprintln!(
                                                "streams: c={ceiling} still fails after settling although this cell \
                                                 carried c={proven_clean} - restarting the gateway to find out whether \
                                                 it stopped serving or the rig is still dirty"
                                            );
                                            match restart_to_rest(spec, &cfg.relaunch_launcher, &cfg.relaunch_commands) {
                                                Ok(()) => stream_window(cfg.gateway_addr, &p.path, &p.body, &p.headers, p.dialect, ceiling)
                                                    .map(|w3| {
                                                        let passed3 = streams_gate_passes(&w3);
                                                        p.points.push(point_of(&w3, passed3));
                                                        passed3
                                                    })
                                                    .unwrap_or(false),
                                                Err(e) => {
                                                    eprintln!("streams: the gateway could not be restarted: {e}");
                                                    false
                                                }
                                            }
                                        }
                                        // No declared relaunch means the harness does not own this
                                        // process and must not bounce it; the honest reading stays
                                        // the rig-side one it already had.
                                        None => false,
                                    };
                                    // Restart is the attribution test: cleared means the old process
                                    // was wedged and the gateway is at fault; not cleared means a
                                    // fresh gateway still can't carry a proven rung, so the host is
                                    // still the variable and it stays ours.
                                    stop = if restart_cleared {
                                        StreamStop::GatewayDidNotRecover {
                                            at: ceiling,
                                            proven: proven_clean,
                                            restart_cleared,
                                        }
                                    } else {
                                        StreamStop::RigContaminated { at: ceiling, proven: proven_clean }
                                    };
                                    break;
                                }
                                if !recovered {
                                    // One vote against the rung, not a verdict on the search. Abandon
                                    // this rung unconfirmed and narrow again on the next pass; if the
                                    // budget runs out first the stop below is what publishes.
                                    stop = StreamStop::SteppedRungFailed { at: ceiling };
                                    seed_failed = true;
                                    continue;
                                }
                            } else {
                                first_fps = w.fps();
                            }
                        }
                        None => {
                            stop = StreamStop::WindowUnavailable { at: ceiling };
                            break;
                        }
                    }
                }
                // The floor the search already proved is a result, not a leftover: the step-down
                // budget can converge toward `proven_clean` without ever re-testing it, publishing
                // nothing even though "could not confirm the higher rung" and "know nothing" are
                // different statements. So when otherwise out of budget, give the floor the same
                // confirmation the bisected rung got (full window set, same majority rule) and
                // publish it as a conservative number if it holds — it cannot inflate a result since
                // `proven_clean` was already driven cleanly and still needs to hold a majority now.
                if winner.is_none() && proven_clean > 0 && stop.floor_fallback_ok() {
                    eprintln!(
                        "streams: the ceiling search is out of budget; confirming the floor this cell \
                         already carried (c={proven_clean}) rather than publishing nothing"
                    );
                    settle_after_streams(ceiling.max(proven_clean) * 2);
                    let mut held = 0usize;
                    let mut total = 0usize;
                    let mut rates: Vec<f64> = Vec::new();
                    for _ in 0..crate::search::WINDOWS_PER_RUNG {
                        if let Some(w) = stream_window(
                            cfg.gateway_addr, &p.path, &p.body, &p.headers, p.dialect, proven_clean,
                        ) {
                            let passed = streams_gate_passes(&w);
                            p.points.push(point_of(&w, passed));
                            total += 1;
                            if passed {
                                held += 1;
                                rates.push(w.fps());
                            }
                        }
                        settle_after_streams(proven_clean);
                    }
                    if stream_ceiling_confirmed(total, held) {
                        rates.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                        if let Some(fps) = crate::search::nearest_rank_median(&rates) {
                            eprintln!("streams: floor c={proven_clean} confirmed at {fps:.0} fps");
                            winner = Some((proven_clean, fps));
                        }
                    } else {
                        // The floor did not hold either, so the absence stands and keeps the reason
                        // the search already had. A rung that cannot be reconfirmed is not a ceiling.
                        eprintln!(
                            "streams: floor c={proven_clean} held {held} of {total} windows - not \
                             confirmed, so the cell keeps its absence"
                        );
                    }
                }
                match winner {
                    Some((conc, fps)) => CellStreams {
                        concurrency: Measurement::Measured(conc),
                        fps: Measurement::Measured(fps),
                        points: p.points,
                    },
                    // The reason names which of the five ways the search actually ended without a
                    // winner (rig shortfall, floor reached, stepped rung failed, window unavailable,
                    // or budget exhausted), rather than one generic sentence that would make a rig
                    // shortfall read as "the gateway did not hold the gate".
                    None => CellStreams {
                        concurrency: Measurement::absent(Absent::NotMeasured),
                        fps: Measurement::absent_because(
                            stop.absent_kind(),
                            stop.describe(c, MAX_CEILING_STEPDOWNS),
                        ),
                        points: p.points,
                    },
                }
            }
            // The search memoises every probe, so the winning rung is always in hand; if it somehow
            // were not, the ceiling publishes with an unmeasured rate rather than an invented one.
            None => CellStreams {
                concurrency: Measurement::Measured(c),
                fps: Measurement::absent_because(
                    Absent::NotMeasured,
                    format!("the stream ceiling c={c} was proven, but its frames/sec reading was not retained"),
                ),
                points: p.points,
            },
        },
        None => CellStreams {
            concurrency: Measurement::absent(r.ceiling.reason().cloned().unwrap_or(Absent::NotMeasured)),
            // The search's own reason AND its evidence travel, exactly as `sweep_sustained_cell`
            // carries `bisect_ceiling`'s.
            fps: match (r.ceiling.reason().cloned(), r.ceiling.detail()) {
                (Some(reason), Some(detail)) => Measurement::absent_because(reason, detail),
                (Some(reason), None) => Measurement::absent(reason),
                (None, _) => Measurement::absent(Absent::NotMeasured),
            },
            points: p.points,
        },
    }
}

// ── cpu_fps: RETIRED ──────────────────────────────────────────────────────────────────────────────
//
// `cpu_fps` ("peak SSE frames/sec" via `saturation_plateau`) is gone: field data showed readings
// that were inverted below the gated `streams_sustained_fps` boundary, redundant with it, or above
// it only because they were measured at a concurrency where the delivery gate did not hold (dropping
// or stalling frames). `streams_sustained_fps` — the rate at the proven delivery boundary — is the
// honest version of the same quantity and stays.

/// The MOCK's own frames/sec at one concurrency, driven straight at it.
///
/// The streaming analogue of `suite::rig_ceiling`. Takes the reference at the operating point the
/// gateway's own number was taken at, since the rig isn't equally fast at every concurrency and a
/// reference from the top of the range would understate the gateway's closeness to it. A single
/// window, not a search — a point measurement makes no turnover claim.
///
/// Takes the mock's address/model/token directly rather than a `RunConfig`, since a stream window
/// needs only where and what to send.
pub fn stream_fps_at(
    mock_addr: SocketAddr,
    model: &str,
    auth: &str,
    dialect: Dialect,
    concurrency: u32,
) -> Measurement<f64> {
    let path = dialect.mock_direct_path(model);
    let body = dialect.stream_body(model);
    // Mock's own auth shape, no gateway routing headers: those select an upstream inside a gateway
    // and mean nothing here.
    let headers = dialect.auth_headers(auth);

    // Reference must be the median of the same window count (`WINDOWS_PER_RUNG`) the observation
    // gets: a single-window reference is a one-sample bar policing a figure built to resist one
    // unlucky window, and an understated reference falsely clears the gateway's real number as
    // unvouchable.
    //
    // Box is also given time to settle first: the reference is taken right after a ladder that just
    // drove thousands of concurrent streams through this host, and residual draining/CPU heat would
    // depress it. The median defends against one unlucky window, not a uniformly busy box.
    //
    // NOT on the live path — `suite::stream_rig_ceiling` uses `mock_frame_ceiling_fps` (pure
    // arithmetic from the mock's declared pacing) instead; nothing outside this file's tests calls
    // this function. Kept as the only measured cross-check on that arithmetic ceiling.
    //
    // Sleep kept short (runs twice per served streaming cell): doesn't need to outlast TIME_WAIT,
    // just the run-queue/socket drain, which clears much faster.
    std::thread::sleep(std::time::Duration::from_secs(2));

    let mut clean: Vec<f64> = Vec::with_capacity(crate::search::WINDOWS_PER_RUNG);
    let mut why: Option<String> = None;
    for _ in 0..crate::search::WINDOWS_PER_RUNG {
        match stream_window(mock_addr, &path, &body, &headers, dialect, concurrency) {
            // Must be a clean window or it is not a ceiling. Logged per window (streams/frames/fps)
            // since "the window was slow" and "the window read fewer frames" are otherwise
            // indistinguishable from the collapsed fps figure alone.
            Some(w) if w.errored == 0 && w.frames > 0 => {
                eprintln!(
                    "[ref] c={concurrency} streams={} frames={} content={}/{} stalls={} elapsed={:.3}s fps={:.0}",
                    w.streams, w.frames, w.content_frames, w.expected_content_frames, w.stalls,
                    w.elapsed_s, w.fps()
                );
                clean.push(w.fps())
            }
            Some(w) => {
                why.get_or_insert(format!(
                    "the direct-to-mock stream window at c={concurrency} was not clean: {} of {} streams failed, {} frames",
                    w.errored, w.streams, w.frames
                ));
            }
            None => {
                why.get_or_insert(format!(
                    "no direct-to-mock stream window ran at c={concurrency}"
                ));
            }
        }
    }
    // A full set, not merely a non-empty one: `median` would happily return a value from a single
    // clean window, silently reintroducing the single-sample bar this function exists to avoid. Same
    // bar as `confirm_ceiling`/`stream_ceiling_confirmed`.
    if clean.len() < crate::search::WINDOWS_PER_RUNG {
        let got = clean.len();
        let want = crate::search::WINDOWS_PER_RUNG;
        return Measurement::absent_because(
            Absent::NotMeasured,
            why.unwrap_or_else(|| {
                format!(
                    "only {got} of {want} direct-to-mock stream windows at c={concurrency} came back \
                     clean, so there is no reference this rig can stand behind - a ceiling taken from \
                     fewer windows than the observation it judges is not a ceiling"
                )
            }),
        );
    }
    // The median is what stops one slow window from deciding a ceiling on its own.
    match crate::stats::median(&clean).value() {
        Some(fps) => Measurement::Measured(*fps),
        None => Measurement::absent_because(
            Absent::NotMeasured,
            why.unwrap_or_else(|| {
                format!("no clean direct-to-mock stream window ran at c={concurrency}")
            }),
        ),
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
    /// Every metric the engine took on this cell, keyed by the artifact field it fills. `None` for
    /// an unserved cell (never asked), distinct from an empty map (measured nothing).
    pub metrics: Option<std::collections::BTreeMap<&'static str, Measurement<f64>>>,
    /// Evidence behind those scalars: throughput rungs and memory readings. `None` alongside
    /// `metrics` for a cell that was never measured, empty for one measured with no series.
    pub series: Option<crate::metric::Series>,
    /// Seconds per metric group (`throughput`, `streaming`, `memory`, ...), so a slow run can be
    /// diagnosed offline rather than re-run with a stopwatch — a single total can't distinguish
    /// which group cost the time. `None` for a cell that was never measured.
    pub timings_s: Option<std::collections::BTreeMap<&'static str, f64>>,
    /// Whether the gateway was proven to have emitted this cell's egress dialect upstream — an
    /// anti-false-positive guard (see `reverify.rs`), not a measurement, hence a plain tri-state
    /// beside the metrics. `Default` for a cell that was never served.
    pub reverify: crate::reverify::Reverified,
}

/// Walk the grid: probe every pairing, sweep the ones that are served.
pub fn run_grid(cfg: &RunConfig, lo: u32, hi: u32) -> Vec<CellResult> {
    run_grid_with(cfg, lo, hi, metric::METRICS)
}

/// The same walk, over an explicit metric list, so a test can drive the grid without performing
/// every real measurement.
pub fn run_grid_with(
    cfg: &RunConfig,
    lo: u32,
    hi: u32,
    metrics: &[&dyn metric::Metric],
) -> Vec<CellResult> {
    let mut out = Vec::new();
    run_grid_streaming(cfg, lo, hi, metrics, &mut |c| out.push(c));
    out
}

/// The same walk, handing each cell over as it finishes rather than at the end. `run_grid_with`'s
/// `Vec` return means the whole grid must finish before the caller sees anything, which breaks the
/// "an interrupted run must not lose already-measured cells" guarantee `suite.rs` depends on to
/// checkpoint per egress column. `run_grid_with` stays as a collecting wrapper for tests.
pub fn run_grid_streaming(
    cfg: &RunConfig,
    lo: u32,
    hi: u32,
    metrics: &[&dyn metric::Metric],
    on_cell: &mut dyn FnMut(CellResult),
) {
    let total_cells = cfg.dialects.len() * cfg.dialects.len();
    let total = total_cells;
    let mut done = 0usize;
    // Set when a mid-grid restart FAILED: from that point the harness cannot vouch for the
    // gateway's state (it may be up but half-configured), so every remaining cell is recorded as
    // untestable naming our failure instead of being measured into a false gateway verdict.
    let mut restart_poisoned: Option<String> = None;
    for eg in &cfg.dialects {
        for ing in &cfg.dialects {
            let id = CellId::new(ing.as_str(), eg.as_str());
            done += 1;
            if let Some(why) = &restart_poisoned {
                eprintln!("[cell {done}/{total}] {id}: untestable (harness: restart failed earlier in the grid)");
                on_cell(CellResult {
                    outcome: CellOutcome::untestable(id, why.clone()),
                    metrics: None,
                    series: None,
                    timings_s: None,
                    reverify: Default::default(),
                });
                continue;
            }
            // A cell the manifest declares out of scope, or the rig cannot pose, is never probed —
            // checked before `probe_cell` runs at all, since sending and discarding the status isn't
            // the same as never sending it (see `RunConfig::matrix`'s doc).
            if crate::manifest::is_untestable_cell(&cfg.untestable_cells, ing.as_str(), eg.as_str())
            {
                let note = if cfg.untestable_note.is_empty() {
                    "the rig cannot pose this pairing".to_string()
                } else {
                    cfg.untestable_note.clone()
                };
                // Logged per cell, not buffered until the grid finishes, so a run that dies mid-way
                // leaves progress in .run.log and can be tailed live.
                eprintln!("[cell {done}/{total}] {id}: untestable");
                on_cell(CellResult {
                    outcome: CellOutcome::untestable(id, note),
                    metrics: None,
                    series: None,
                    timings_s: None,
                    reverify: Default::default(),
                });
                continue;
            }
            if crate::manifest::matrix_declared_capable(&cfg.matrix, ing.as_str(), eg.as_str())
                == Some(false)
            {
                let note = if cfg.matrix_note.is_empty() {
                    format!(
                        "{} is not one of this gateway's declared capable pairings",
                        id
                    )
                } else {
                    cfg.matrix_note.clone()
                };
                eprintln!("[cell {done}/{total}] {id}: not_configurable");
                on_cell(CellResult {
                    outcome: CellOutcome::not_configurable(id, note),
                    metrics: None,
                    series: None,
                    timings_s: None,
                    reverify: Default::default(),
                });
                continue;
            }
            // The rig is re-confirmed for every cell, not once for the whole grid: `mock_healthy`
            // feeds `persistent_transient_verdict`'s NotVerified, and reading it once before an
            // hours-long grid would let a mid-run mock degradation grade every later cell as though
            // the rig were fine. One cheap request per cell against the alternative.
            let healthy = mock_healthy(cfg);
            if !healthy {
                eprintln!("[cell {done}/{total}] {id}: the mock did not answer its own health check - nothing observed here is attributable to the gateway");
            }
            let mut served = probe_cell(cfg, &id, healthy);
            // A gateway that died takes the rest of the grid with it unless restarted here:
            // `probe_cell`'s retry budget outlasts a merely-busy gateway but not a gone one, and
            // nothing else between cells restarts the process. The harness owns the gateway's
            // lifetime (`relaunch`), so bring it back and ask once more before writing off the grid.
            if let (Served::Untestable(ref why), Some(spec)) = (&served, cfg.relaunch.as_ref()) {
                if why.contains("no connection") {
                    eprintln!("[cell {done}/{total}] {id}: {why} - restarting the gateway before writing off the rest of the grid");
                    match restart_to_rest(spec, &cfg.relaunch_launcher, &cfg.relaunch_commands) {
                        Ok(()) => {
                            served = probe_cell(cfg, &id, healthy);
                            eprintln!(
                                "[cell {done}/{total}] {id}: after restart, {}",
                                if served.is_measurable() {
                                    "it answers"
                                } else {
                                    "still not answering"
                                }
                            );
                        }
                        // A failed restart poisons everything after it: the gateway may now answer
                        // while half-configured, the worst state to keep measuring against. The rest
                        // of the grid is marked untestable naming our own failure instead.
                        Err(e) => {
                            eprintln!(
                                "[cell {done}/{total}] {id}: the gateway could not be restarted: {e} - refusing to measure the rest of the grid against an unvouched process"
                            );
                            restart_poisoned = Some(format!(
                                "a mid-grid restart failed ({e}), so the harness can no longer vouch for the gateway's state; measuring on would publish our failure as the gateway's"
                            ));
                        }
                    }
                }
            }
            // If the cell is served, run every metric in `metric::METRICS` — one source of truth, so
            // a measurement can't be implemented and never wired in.
            // Re-verify before measuring, not after: the metrics drive millions of requests through
            // the same recorder the check reads, and the recorder's `body_ok` only describes the
            // last body it saw, so this needs a cleared recorder with nothing else in flight.
            let reverify = if served.is_measurable() {
                crate::reverify::reverify_cell(cfg, &id, *ing)
            } else {
                Default::default()
            };
            let (metrics, series, timings) = if served.is_measurable() {
                let ctx = metric::CellCtx {
                    cfg,
                    id: &id,
                    dialect: *ing,
                    min_conc: lo,
                    max_conc: hi,
                };
                let (m, s, t) = metric::process_cell_with(&ctx, metrics);
                // Per-metric-group timing breakdown, one greppable line, so a slow cell can be
                // diagnosed without re-running.
                let mut by_cost: Vec<_> = t.iter().collect();
                by_cost.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));
                let total: f64 = t.values().sum();
                let breakdown = by_cost
                    .iter()
                    .map(|(name, secs)| format!("{name}={secs:.1}s"))
                    .collect::<Vec<_>>()
                    .join(" ");
                // Logged under `[cost]`, not `[cell]`: the status board parses `[cell N/M] <id>:
                // <verdict>` to count served cells, and reusing that prefix here would double-count.
                eprintln!("[cost {done}/{total_cells}] {id}: {total:.1}s total | {breakdown}");
                (Some(m), Some(s), Some(t))
            } else {
                (None, None, None)
            };
            let outcome = match served {
                Served::Yes => CellOutcome::served(id),
                Served::No(v, ev) => {
                    let n = format!("probed and answered {} (HTTP {})", v.token(), ev.status);
                    CellOutcome::not_served(id, v, ev, n)
                }
                Served::Untestable(r) => CellOutcome::untestable(id, r),
                Served::NotConfigurable(r) => CellOutcome::not_configurable(id, r),
                Served::UnprobedAuth(ev) => CellOutcome::unprobed_auth(id, ev),
            };
            let label = match &outcome.served {
                Served::Yes => "served".to_string(),
                Served::No(v, ev) => format!("{} (HTTP {})", v.token(), ev.status),
                Served::Untestable(_) => "untestable".to_string(),
                Served::NotConfigurable(_) => "not_configurable".to_string(),
                Served::UnprobedAuth(ev) => format!("unprobed_auth (HTTP {})", ev.status),
            };
            eprintln!("[cell {done}/{total}] {}: {label}", outcome.id);
            on_cell(CellResult {
                outcome,
                metrics,
                series,
                timings_s: timings,
                reverify,
            });
        }
    }
}

/// A minimal `RunConfig` for tests across this crate, mirroring `manifest::test_fixture` (one place
/// to add a field, not one per call site). `matrix`/`untestable_cells` empty: undeclared, so every
/// cell is probed unless a test overrides them via struct-update syntax.
#[cfg(test)]
pub(crate) fn test_fixture(gw: SocketAddr, mock: SocketAddr) -> RunConfig {
    RunConfig {
        // The test fixture pins nothing, so it declares no cores: utilisation is absent rather than
        // a figure borrowed from whatever else the test host happens to be running.
        gw_cores: String::new(),
        gateway_addr: gw,
        mock_addr: mock,
        model: "m".into(),
        egress_models: Default::default(),
        auth: "dummy".into(),
        dialects: vec![Dialect::Openai],
        sweep_duration_s: 1,
        probe_timeout: Duration::from_secs(2),
        load_cores: None,
        static_headers: Vec::new(),
        egress_headers: Default::default(),
        runtime: crate::manifest::Runtime::Native {
            proc_match: "test-fixture".into(),
        },
        declared_path: String::new(),
        cell_paths: Default::default(),
        matrix: Vec::new(),
        matrix_note: String::new(),
        untestable_cells: Vec::new(),
        untestable_note: String::new(),
        relaunch: None,
        relaunch_commands: Vec::new(),
        relaunch_launcher: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    // The drain waits on a condition, and the condition must be reachable on a box that's never
    // perfectly idle, or the 60s backstop becomes the schedule for every large window.
    #[test]
    fn the_drain_target_is_reachable_on_a_box_that_is_never_perfectly_idle() {
        // The baseline is the cell's own starting count, scaled - not a number chosen here. A box
        // that starts with 200 sockets in TIME_WAIT must not be asked to return to 0.
        let quiet = 200u64;
        let target = quiet * STREAM_SETTLE_TW_TOLERANCE + 64;
        assert!(target > quiet, "the target must sit ABOVE the level the cell started at");
        // The mock and the load generator hold their own sockets throughout, so a window that closes
        // cleanly still leaves the box above where it began. That has to read as drained.
        assert!(quiet + 100 <= target, "ordinary residue still counts as drained");
        // And a box still holding thousands of dying sockets from a c=8,192 window does NOT.
        assert!(quiet + 8_000 > target, "a real backlog is not mistaken for quiet");

        // An idle box reads 0 and must still get a usable target rather than an impossible one.
        assert_eq!(0 * STREAM_SETTLE_TW_TOLERANCE + 64, 64, "an idle box still has headroom");
    }

    // The backstop is one TIME_WAIT generation: a closed socket can't need longer, past that the
    // host is broken rather than busy.
    #[test]
    fn the_settle_backstop_covers_a_full_time_wait_generation() {
        assert!(
            STREAM_SETTLE_MAX_MS >= 60_000,
            "a backstop under one TIME_WAIT generation cannot absorb the residue it exists for"
        );
        // The poll has to be fine enough that a small rung's wait is milliseconds, not seconds: the
        // whole gain over the old fixed sleep is that a rung which drains at once stops paying.
        assert!(
            STREAM_SETTLE_POLL_MS <= 100,
            "a coarse poll would reintroduce the fixed cost this replaced"
        );
    }

    // The floor fallback's gate: only meaningful when the host itself wasn't the suspect for the
    // rungs that just failed.
    #[test]
    fn floor_fallback_only_where_the_host_is_not_the_suspect() {
        // Search ran out of room or budget: gateway and rig both innocent so far.
        assert!(StreamStop::BudgetExhausted.floor_fallback_ok());
        assert!(StreamStop::FloorReached { last: 4193 }.floor_fallback_ok());
        assert!(StreamStop::SteppedRungFailed { at: 4193 }.floor_fallback_ok());

        // Our instrument is the variable here.
        assert!(!StreamStop::RigRanShort { measured: 1, wanted: 3 }.floor_fallback_ok());
        assert!(!StreamStop::WindowUnavailable { at: 4096 }.floor_fallback_ok());
        assert!(!StreamStop::RigContaminated { at: 4096, proven: 4096 }.floor_fallback_ok());

        // A restarted gateway is a different process than the one the sweep measured.
        assert!(!StreamStop::GatewayDidNotRecover { at: 8192, proven: 4096, restart_cleared: true }
            .floor_fallback_ok());
        assert!(!StreamStop::GatewayDidNotRecover { at: 8192, proven: 4096, restart_cleared: false }
            .floor_fallback_ok());
    }

    // The floor still has to earn it via the same majority rule as any other repeated measurement.
    #[test]
    fn the_floor_is_published_only_on_the_same_majority_every_other_rate_needs() {
        assert!(stream_ceiling_confirmed(3, 3), "three of three holds");
        assert!(stream_ceiling_confirmed(3, 2), "two of three is a majority");
        assert!(!stream_ceiling_confirmed(3, 1), "one of three is the busbar c=5,652 shape");
        assert!(!stream_ceiling_confirmed(3, 0), "none of three publishes nothing");
        assert!(!stream_ceiling_confirmed(2, 2), "a short window set is not a confirmation");
    }

    #[test]
    fn the_stream_bound_is_physical_and_the_runaway_cap_never_participates() {
        // A cap chosen near where measurements live becomes part of the measurement — the real
        // stopping condition must be measured (see
        // `search::a_ladder_that_ends_on_failing_rungs_publishes_the_best_passing_rung`), and the cap
        // here is only a runaway backstop.
        let bench_box_fds = 1_048_576;
        let bench_box_ports = 32_768;
        assert_eq!(
            super::stream_ceiling_from(bench_box_fds, bench_box_ports),
            bench_box_ports,
            "the PHYSICAL bound must decide it - with descriptors raised far above the port range, \
             the port range is the answer and the runaway cap must not be visible in it"
        );
        // Asserted through the function, not against the constant directly, so this can't become a
        // dead compile-time-literal comparison.
        let unbounded_host = super::stream_ceiling_from(u32::MAX, u32::MAX);
        assert_eq!(
            unbounded_host,
            super::STREAM_RUNAWAY_CAP,
            "with no physical limit the runaway backstop must be what bounds the ladder"
        );
        assert!(
            unbounded_host >= 4 * 16_384,
            "the backstop must sit far above the highest rung the field has cleanly sustained \
             (apisix, c=16384), or it is a measurement bound wearing a safety label: {unbounded_host}"
        );
        // Descriptors still bind when they are genuinely the smaller number: a stock 1024-descriptor
        // box must not be asked for thousands of held-open streams, each costing one descriptor on the
        // rig, one on the gateway and one on the mock.
        assert!(
            super::stream_ceiling_from(1024, bench_box_ports) <= 512,
            "a stock descriptor limit is a real bound and must still bite"
        );
        let streams = super::stream_connection_ceiling();
        assert!(
            streams >= 1,
            "a ceiling of zero would measure nothing: {streams}"
        );
        assert!(
            streams.is_power_of_two(),
            "the ladder doubles, so a ceiling off the ladder is never actually reached: {streams}"
        );
    }

    // A reference must rest on as many windows as the number it judges — otherwise `stats::median`
    // would happily return a value from one clean window, reintroducing the single-sample bar the
    // median was meant to remove.
    #[test]
    fn a_rig_reference_needs_a_full_set_of_clean_windows() {
        let want = crate::search::WINDOWS_PER_RUNG;
        // The rule as the function applies it: fewer clean windows than a rung is measured with means
        // no reference, however many of them happened to be fast.
        let enough = |clean: usize| clean >= want;
        assert!(
            !enough(1),
            "one clean window is the bar the median was supposed to raise"
        );
        assert!(
            !enough(want - 1),
            "one short is still short of what the observation rests on"
        );
        assert!(
            enough(want),
            "a full set is the same bar every other rate on the board meets"
        );
        assert!(enough(want + 1), "and more is fine");
    }

    // A pacing interval of zero would fail every stream: `stall_bound_us()` is this times
    // STREAM_STALL_MULTIPLIER, so 0 makes every inter-frame gap count as a stall.
    #[test]
    fn a_zero_pacing_interval_is_refused_in_favour_of_the_default() {
        // Driven through the parse+filter chain this function uses, since the function itself reads a
        // process-global the test suite shares.
        let read = |raw: &str| -> u64 {
            raw.trim()
                .parse::<u64>()
                .ok()
                .filter(|v| *v > 0)
                .unwrap_or(20)
        };
        assert_eq!(
            read("0"),
            20,
            "zero is not a pace and must fall back to the default"
        );
        assert_eq!(read("  0  "), 20, "including with whitespace around it");
        assert_eq!(read("40"), 40, "a real value is still honoured");
        assert_eq!(
            read(" 40 "),
            40,
            "and still trimmed, matching the mock's own reader"
        );
        assert_eq!(
            read("nonsense"),
            20,
            "and an unparseable value still defaults"
        );
        assert!(
            super::stream_pacing_interval_ms() > 0,
            "the live reader can never return zero"
        );
    }

    // One lucky window must not become a confirmed stream ceiling: the majority test alone (without
    // a minimum-window check) would let two skipped/absent repeats leave 1-of-1 read as a pass.
    #[test]
    fn a_stream_ceiling_needs_as_many_windows_as_any_other_rung() {
        let n = crate::search::WINDOWS_PER_RUNG;
        assert!(
            !super::stream_ceiling_confirmed(1, 1),
            "1 of 1 is the bisection's own window with both repeats absent - not a confirmation"
        );
        assert!(
            !super::stream_ceiling_confirmed(2, 2),
            "2 of 2 is still short of a climb rung: one absent repeat must not lower the bar"
        );
        // A full set that genuinely held publishes.
        assert!(
            super::stream_ceiling_confirmed(n, n),
            "every window held: publish"
        );
        // A full set with a real majority publishes; a real minority does not. This is the half that
        // already worked, asserted so a fix to the other half cannot quietly remove it.
        assert!(
            super::stream_ceiling_confirmed(3, 2),
            "2 of 3 held is a majority"
        );
        assert!(!super::stream_ceiling_confirmed(3, 1), "1 of 3 held is not");
    }

    // A window that lost lanes must be discarded, loudly: survivors alone aren't good enough, since
    // `streams`/`expected_frames` are accumulated per surviving lane, so a panicked lane removes
    // itself from both numerator and denominator and flatters the delivery ratio.
    #[test]
    fn a_window_that_lost_lanes_is_refused_and_says_so() {
        let why = super::window_refusal(4, 60, 64).expect("lost lanes must refuse the window");
        assert!(
            why.contains("PANICKED"),
            "the refusal must name the fault: {why}"
        );
        assert!(
            why.contains('4') && why.contains("64"),
            "and must quantify it - how many of how many: {why}"
        );

        // Even ONE lost lane. The survivors' ratio is not this window's ratio, and there is no
        // threshold below which a measurement may quietly describe a different window than it ran.
        assert!(
            super::window_refusal(1, 63, 64).is_some(),
            "one lost lane still means the window did not run at the concurrency it claims"
        );

        // Nothing came back at all, nothing panicked: unmeasured, and it must still SAY so rather
        // than returning in silence the way the old path did.
        let why = super::window_refusal(0, 0, 8).expect("an empty window must refuse");
        assert!(why.contains("no streams"), "{why}");

        // And the case that must still be PUBLISHED: every lane came home. A refusal rule that
        // refuses everything is as useless as one that refuses nothing.
        assert_eq!(
            super::window_refusal(0, 64, 64),
            None,
            "a whole window is a real measurement and must not be thrown away"
        );
        assert_eq!(super::window_refusal(0, 1, 1), None);
    }

    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    pub(super) fn cfg_for(gw: SocketAddr, mock: SocketAddr) -> RunConfig {
        test_fixture(gw, mock)
    }

    // ---- restart_to_rest must not leak the process it replaces --------------------------------

    /// `ps -o state=` reports a zombie as `Z` on both Linux and macOS. Empty output (or a nonzero
    /// exit) means the OS has no process table entry for that pid at all.
    fn ps_state(pid: u32) -> String {
        std::process::Command::new("ps")
            .args(["-o", "state=", "-p", &pid.to_string()])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    }

    /// A native "gateway" that actually listens, so `wait_until_ready` (real TCP, inside
    /// `restart_to_rest`) has something honest to observe - a marker unique to this test process so
    /// `pkill -f`/`pgrep -f` can never match an unrelated process on a shared runner.
    fn listening_native_spec(port: u16, marker: &str) -> crate::launch::LaunchSpec {
        let script = "import socket,sys
s=socket.socket(socket.AF_INET, socket.SOCK_STREAM)
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(('127.0.0.1', int(sys.argv[1])))
s.listen(1)
while True:
    conn, _ = s.accept()
    conn.close()
";
        crate::launch::LaunchSpec {
            runtime: crate::manifest::Runtime::Native {
                proc_match: marker.to_string(),
            },
            kind: crate::launch::LaunchKind::Native {
                binary: "python3".into(),
                args: vec![
                    "-c".into(),
                    script.into(),
                    port.to_string(),
                    marker.to_string(),
                ],
                env: vec![],
                env_unset: vec![],
            },
            cores: "0".into(),
            port,
            ready_budget: Duration::from_secs(5),
            boot_backoff: Duration::from_millis(100),
            pre_launch: None,
        }
    }

    // Guards against `restart_to_rest` leaving the process a prior restart replaced as a zombie.
    #[test]
    fn restart_to_rest_reaps_the_process_it_replaces() {
        // `build_invocation` pins with `taskset`, Linux-only; skip loudly on other platforms rather
        // than failing for an unrelated reason.
        if std::process::Command::new("sh")
            .args(["-c", "command -v taskset"])
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("skipping restart_to_rest_reaps_the_process_it_replaces: no taskset on this platform (the field and CI are Linux, where this runs for real)");
            return;
        }
        let marker = format!("otb-test-restart-to-rest-{}", std::process::id());
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port to pick one");
            l.local_addr().expect("addr").port()
        };
        let spec = listening_native_spec(port, &marker);
        let launcher: std::sync::Mutex<crate::launch::RealLauncher> = Default::default();

        restart_to_rest(&spec, &launcher, &[]).expect("the first restart must come up");
        let pid1 = launcher
            .lock()
            .expect("lock")
            .native_pid()
            .expect("a native child must be tracked");

        restart_to_rest(&spec, &launcher, &[]).expect("the second restart must come up");

        assert!(
            !ps_state(pid1).contains('Z'),
            "the process the second restart replaced must be reaped, not left as a zombie; ps state was {:?}",
            ps_state(pid1)
        );

        let _ = crate::supervise::stop_and_wait(&spec.runtime, spec.port, Duration::from_secs(5));
    }

    // Guards against the unconfigured-relaunch defect: `restart_to_rest` must replay post-boot
    // commands (config written via admin API dies with a docker `rm -f` stop), and a failing command
    // must fail the restart rather than leave a half-configured gateway answering probes.
    #[test]
    fn restart_to_rest_replays_the_post_boot_commands_and_fails_when_they_do() {
        if std::process::Command::new("sh")
            .args(["-c", "command -v taskset"])
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            eprintln!("skipping restart_to_rest_replays_the_post_boot_commands_and_fails_when_they_do: no taskset on this platform");
            return;
        }
        let marker = format!("otb-test-restart-commands-{}", std::process::id());
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port to pick one");
            l.local_addr().expect("addr").port()
        };
        let spec = listening_native_spec(port, &marker);
        let launcher: std::sync::Mutex<crate::launch::RealLauncher> = Default::default();

        // A witness file only the replayed command writes: its existence IS the replay.
        let witness = std::env::temp_dir().join(format!("{marker}.witness"));
        let _ = std::fs::remove_file(&witness);
        let write = format!("touch {}", witness.display());
        restart_to_rest(&spec, &launcher, &[write])
            .expect("a restart whose post-boot command succeeds must come up");
        assert!(
            witness.exists(),
            "the restart must REPLAY the post-boot commands, not just relaunch the process"
        );
        let _ = std::fs::remove_file(&witness);

        // And a failing command is the restart failing, loudly, naming the command.
        let err = restart_to_rest(&spec, &launcher, &["false".to_string()])
            .expect_err("a failing post-boot command must fail the restart");
        assert!(
            err.contains("post-boot configuration failed"),
            "the error must name the configuration stage, got: {err}"
        );

        let _ = crate::supervise::stop_and_wait(&spec.runtime, spec.port, Duration::from_secs(5));
    }

    // Prevents the collapsed egress axis: a fixed model would make all six egress columns reach the
    // same upstream, publishing six cells for one actual measurement.
    #[test]
    fn every_egress_column_asks_for_its_own_model() {
        let a = "127.0.0.1:1".parse().expect("addr");
        let mut cfg = test_fixture(a, a);
        cfg.model = "canonical".into();
        cfg.egress_models = [("anthropic".to_string(), "claude-x".to_string())]
            .into_iter()
            .collect();

        assert_eq!(
            model_for(&cfg, "anthropic"),
            "claude-x",
            "the column's own name must be sent"
        );
        // An egress the manifest names nothing for falls back to the declared model: right for a
        // single-upstream gateway, and for the column whose canonical name already IS that model.
        assert_eq!(model_for(&cfg, "openai"), "canonical");
    }

    // The model rides in the PATH for two of the six dialects, so a per-egress model that never
    // reached `path_for` would still collapse those columns even with the body correct.
    #[test]
    fn a_path_that_embeds_the_model_embeds_the_egress_columns_model() {
        let a = "127.0.0.1:1".parse().expect("addr");
        let mut cfg = test_fixture(a, a);
        cfg.model = "canonical".into();
        cfg.egress_models = [("bedrock".to_string(), "vendor.model-v1:0".to_string())]
            .into_iter()
            .collect();

        let p = path_for(&cfg, Dialect::Bedrock, "bedrock");
        assert!(
            p.contains("vendor.model-v1:0"),
            "bedrock's path must carry its own column's model: {p}"
        );
        assert!(
            !p.contains("canonical"),
            "the declared model must not leak into another column: {p}"
        );
    }

    /// An SSE "gateway" whose behaviour is scripted per CONNECTION: the `pass` predicate is handed
    /// the 1-based accept-order index of each connection, and a passing connection streams the full
    /// frame budget while a failing one delivers a single frame and closes (a delivery shortfall,
    /// which fails the streams gate without counting as an errored stream). Windows run one after
    /// another and every lane of a window connects before the next window starts, so accept order
    /// maps connections to windows deterministically.
    fn sse_ladder_server(pass: impl Fn(usize) -> bool + Send + Sync + 'static) -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        let pass = std::sync::Arc::new(pass);
        std::thread::spawn(move || {
            let mut conn_no = 0usize;
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                conn_no += 1;
                let n = conn_no;
                let pass = std::sync::Arc::clone(&pass);
                std::thread::spawn(move || {
                    let mut b = [0u8; 8192];
                    let _ = c.read(&mut b);
                    let _ =
                        c.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n");
                    // Openai-shaped role head (no token) then content deltas: the delivery gate
                    // counts content frames, so bare `data: f0` events would fail for the wrong reason.
                    let _ = c.write_all(
                        b"data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n",
                    );
                    let frames = if pass(n) {
                        crate::metric::STREAM_FRAME_BUDGET
                    } else {
                        1
                    };
                    for i in 0..frames {
                        // Stop at the first failed write, or a dropped lane holds up the whole
                        // window's timing.
                        if c.write_all(
                            format!(
                                "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"f{i}\"}}}}]}}\n\n"
                            )
                            .as_bytes(),
                        )
                        .is_err()
                        {
                            return;
                        }
                    }
                    // Then close: a passing lane has already hit the client's frame budget, and a
                    // failing lane's early close is what makes the shortfall visible at once.
                });
            }
        });
        addr
    }

    #[test]
    fn a_stepped_down_stream_rung_whose_fresh_window_fails_never_votes_for_itself() {
        let gw = sse_ladder_server(|n| n <= 3 || n >= 16);
        let cfg = cfg_for(gw, gw);
        let id = CellId::new("openai", "openai");
        let r = sweep_streams_cell(&cfg, &id, 1, 4);
        assert_ne!(
            r.concurrency.value().copied(),
            Some(1),
            "the stepped-down rung must not publish on the strength of its own failing seed window"
        );
        // Publishes the floor it actually carried (c=2) rather than an absence, via the same
        // confirmation the bisected rung gets; the stepped rung that couldn't seed (c=1) still never
        // votes for itself, which is the invariant this test is named for.
        assert_eq!(
            r.concurrency.value().copied(),
            Some(2),
            "the floor this cell proved on its own ascent is published, not the rung that failed to seed"
        );
        assert!(
            r.fps.value().is_some_and(|v| *v > 0.0),
            "a published ceiling carries the rate that was measured at it: {:?}",
            r.fps
        );
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
        assert_eq!(
            probe_cell(&cfg, &CellId::new("openai", "openai"), true),
            Served::Yes
        );
    }

    // A real error status with a healthy rig is the GATEWAY's own answer about this pairing.
    #[test]
    fn a_real_error_status_with_a_healthy_rig_is_the_gateways_answer() {
        let gw = serve(404);
        let cfg = cfg_for(gw, gw);
        let s = probe_cell(&cfg, &CellId::new("openai", "openai"), true);
        assert!(matches!(s, Served::No(..)), "got {s:?}");
    }

    // 404 and a genuine rejection (e.g. 401) must NOT carry the same verdict end to end through
    // probe_cell: a 404 means the pairing does not exist; a 401 means the gateway reached this
    // pairing and declined the specific request. Collapsing them would publish a gateway that
    // supports a pairing but rejects the probe's auth as indistinguishable from one that never
    // built the route at all.
    #[test]
    fn a_not_found_and_a_rejection_carry_different_verdicts_through_probe_cell() {
        let not_found = serve(404);
        let cfg = cfg_for(not_found, not_found);
        let s = probe_cell(&cfg, &CellId::new("openai", "openai"), true);
        assert!(
            matches!(s, Served::No(ref v, _) if *v == crate::probe::Verdict::NotConfigured),
            "404 must be NotConfigured, got {s:?}"
        );

        let rejected = serve(401);
        let cfg = cfg_for(rejected, rejected);
        let s = probe_cell(&cfg, &CellId::new("openai", "openai"), true);
        assert!(
            matches!(s, Served::No(ref v, ref ev) if *v == crate::probe::Verdict::Failed && ev.status == 401),
            "401 must be Failed with the real status carried as evidence, got {s:?}"
        );
    }

    // THE RED WE WOULD NOT HAVE EARNED. On a dialect whose real clients sign their requests, the
    // harness sends a bearer token and refuses to forge a signature, so a gateway that checks
    // credentials properly answers 401/403 - correctly. Publishing that as a refusal would state
    // that somebody's product does not support a pairing, on evidence that is entirely about our own
    // instrument. The SAME status on a dialect we CAN authenticate stays a real refusal, because
    // there the gateway is answering about the request rather than about our credential.
    #[test]
    fn a_refusal_of_a_credential_the_rig_cannot_sign_is_unprobed_never_a_failure() {
        for status in [401, 403] {
            let gw = serve(status);
            let cfg = cfg_for(gw, gw);

            let s = probe_cell(&cfg, &CellId::new("bedrock", "bedrock"), true);
            assert!(
                matches!(s, Served::UnprobedAuth(ref ev) if ev.status == status),
                "a signed dialect's {status} must be unprobed with its evidence, got {s:?}"
            );

            let s = probe_cell(&cfg, &CellId::new("openai", "openai"), true);
            assert!(
                matches!(s, Served::No(ref v, _) if *v == crate::probe::Verdict::Failed),
                "a dialect the rig CAN authenticate keeps {status} as a real refusal, got {s:?}"
            );
        }
    }

    // The SAME status with an unhealthy rig says nothing about the gateway, so it must not be
    // recorded as the gateway refusing. This is the rig/gateway distinction the board rests on.
    #[test]
    fn the_same_status_with_an_unhealthy_rig_is_not_blamed_on_the_gateway() {
        let gw = serve(404);
        let cfg = cfg_for(gw, gw);
        let s = probe_cell(&cfg, &CellId::new("openai", "openai"), false);
        assert!(
            !matches!(s, Served::No(..)),
            "an unconfirmed rig cannot convict the gateway: {s:?}"
        );
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
        // Empty metric list: this test is about the shape of the grid, not real measurement cost.
        let rows = run_grid_with(&cfg, 1, 2, &[]);
        assert_eq!(rows.len(), 4);
    }

    /// A relaunch spec that cannot come back up (nonexistent binary), so `restart_to_rest` fails
    /// fast on any platform — the failure path is what's under test, no taskset gating needed.
    fn unlaunchable_spec(marker: &str) -> crate::launch::LaunchSpec {
        let port = {
            let l = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port to pick one");
            l.local_addr().expect("addr").port()
        };
        crate::launch::LaunchSpec {
            runtime: crate::manifest::Runtime::Native {
                proc_match: marker.to_string(),
            },
            kind: crate::launch::LaunchKind::Native {
                binary: "/nonexistent-otb-gateway-binary".into(),
                args: vec![marker.to_string()],
                env: vec![],
                env_unset: vec![],
            },
            cores: "0".into(),
            port,
            ready_budget: Duration::from_millis(200),
            boot_backoff: Duration::from_millis(10),
            pre_launch: None,
        }
    }

    // Pins the unvouched-process rule: after a failed mid-grid restart, every remaining cell must be
    // recorded untestable naming the harness's failure, never probed or measured.
    #[test]
    fn a_failed_mid_grid_restart_marks_every_remaining_cell_untestable_naming_the_harness() {
        // Nothing listens at the gateway address, so the first cell's probe ends "no connection",
        // which is exactly the verdict that triggers the restart attempt; the spec then cannot
        // launch, so the restart fails and the grid is poisoned from cell 2 onward.
        let dead: SocketAddr = "127.0.0.1:1".parse().expect("literal");
        let mock = serve(200);
        let mut cfg = cfg_for(dead, mock);
        cfg.dialects = vec![Dialect::Openai, Dialect::Anthropic];
        let marker = format!("otb-test-grid-poison-{}", std::process::id());
        cfg.relaunch = Some(unlaunchable_spec(&marker));

        let rows = run_grid_with(&cfg, 1, 2, &[]);
        assert_eq!(rows.len(), 4, "a poisoned grid still records every pairing");

        // The cell that observed the failure keeps its own honest verdict.
        match &rows[0].outcome.served {
            Served::Untestable(why) => assert!(
                why.contains("no connection"),
                "the first cell keeps the probe's own verdict, got: {why}"
            ),
            other => panic!("the first cell must be untestable, got {other:?}"),
        }
        // Every cell after it is untestable because of OUR restart, and says so.
        for row in &rows[1..] {
            match &row.outcome.served {
                Served::Untestable(why) => {
                    assert!(
                        why.contains("restart failed"),
                        "{}: a poisoned cell must name the failed restart, got: {why}",
                        row.outcome.id
                    );
                    assert!(
                        why.contains("vouch"),
                        "{}: the detail must say the harness cannot vouch for the gateway, got: {why}",
                        row.outcome.id
                    );
                }
                other => panic!(
                    "{}: every cell after a failed restart must be untestable, got {other:?}",
                    row.outcome.id
                ),
            }
            assert!(
                row.metrics.is_none(),
                "{}: a poisoned cell must never be measured",
                row.outcome.id
            );
        }
    }

    // A cell the manifest declares out of its capability grid must never be probed, even if the
    // server behind it would answer 200 — the declaration wins unconditionally.
    #[test]
    fn a_declared_incapable_cell_is_never_probed_even_when_the_server_would_serve_it() {
        let gw = serve(200);
        let mut cfg = cfg_for(gw, gw);
        cfg.dialects = vec![Dialect::Openai, Dialect::Anthropic];
        // Rows = ingress, cols = egress, axis order [openai, openai-responses, anthropic, gemini,
        // cohere, bedrock]: openai->openai capable, openai->anthropic not; anthropic row all not.
        cfg.matrix = vec![
            "100000".into(),
            "000000".into(),
            "000000".into(),
            "000000".into(),
            "000000".into(),
            "000000".into(),
        ];
        cfg.matrix_note = "test: declared capability".into();
        let rows = run_grid_with(&cfg, 1, 2, &[]);
        assert_eq!(
            rows.len(),
            4,
            "every pairing still appears, declared or not"
        );

        let openai_openai = rows
            .iter()
            .find(|r| r.outcome.id.ingress == "openai" && r.outcome.id.egress == "openai")
            .unwrap();
        assert_eq!(
            openai_openai.outcome.served,
            Served::Yes,
            "the declared-capable cell was actually probed"
        );

        for r in rows
            .iter()
            .filter(|r| !(r.outcome.id.ingress == "openai" && r.outcome.id.egress == "openai"))
        {
            assert!(
                matches!(r.outcome.served, Served::NotConfigurable(_)),
                "{} is outside the declared grid and must read not_configurable without ever being probed, got {:?}",
                r.outcome.id, r.outcome.served
            );
        }
    }

    // An unserved cell carries NO metrics. A number attached to a pairing the gateway does not serve
    // is a number about nothing.
    #[test]
    fn an_unserved_cell_carries_no_metrics() {
        let gw = serve(404);
        let cfg = cfg_for(gw, gw);
        let rows = run_grid_with(&cfg, 1, 2, &[]);
        for r in &rows {
            assert!(
                r.metrics.is_none(),
                "{} must not carry metrics",
                r.outcome.id
            );
        }
    }
    // Which URL a cell is driven at, in precedence order: cell-specific path, then declared path
    // (for gateways mounting their compatible API elsewhere), then the dialect standard.
    #[test]
    fn a_cell_is_driven_at_its_own_path_then_the_declared_one_then_the_standard() {
        let mut cfg = cfg_for(
            "127.0.0.1:1".parse().unwrap(),
            "127.0.0.1:2".parse().unwrap(),
        );

        // Nothing declared: the dialect's standard path.
        assert_eq!(
            path_for(&cfg, Dialect::Openai, "openai"),
            "/v1/chat/completions"
        );

        // A declared path that is a longer form of the standard one applies to that dialect, and to
        // that dialect only: a gateway mounting its OpenAI API under a prefix has not moved anyone
        // else's API.
        cfg.declared_path = "/openai/v1/chat/completions".to_string();
        assert_eq!(
            path_for(&cfg, Dialect::Openai, "anthropic"),
            "/openai/v1/chat/completions"
        );
        assert_eq!(
            path_for(&cfg, Dialect::Anthropic, "anthropic"),
            "/v1/messages"
        );

        // A cell's own path wins over both, and ONLY for that cell. The neighbouring cell in the
        // same row keeps the declared path, which is what stops one entrant being measured on a
        // provider-pinned route while the rest of its row is measured on the unified one.
        cfg.cell_paths
            .insert("openai>openai".to_string(), "/passthrough".to_string());
        assert_eq!(path_for(&cfg, Dialect::Openai, "openai"), "/passthrough");
        assert_eq!(
            path_for(&cfg, Dialect::Openai, "anthropic"),
            "/openai/v1/chat/completions"
        );
    }

    // ── sustained_gate_passes: the README's own "p99 under the ceiling AND <0.1% error rate" ──────

    #[test]
    fn a_clean_window_comfortably_under_the_ceiling_passes() {
        assert!(sustained_gate_passes(Some(5_000), 10_000, 0));
    }

    #[test]
    fn a_window_at_or_over_the_p99_ceiling_fails_even_with_zero_errors() {
        assert!(
            !sustained_gate_passes(Some(SUSTAINED_P99_CEILING_US), 10_000, 0),
            "the ceiling itself must not pass"
        );
        assert!(!sustained_gate_passes(
            Some(SUSTAINED_P99_CEILING_US + 1),
            10_000,
            0
        ));
        assert!(
            sustained_gate_passes(Some(SUSTAINED_P99_CEILING_US - 1), 10_000, 0),
            "just under the ceiling passes"
        );
    }

    #[test]
    fn no_p99_reading_never_passes_regardless_of_the_error_rate() {
        assert!(
            !sustained_gate_passes(None, 10_000, 0),
            "an unmeasured latency has not earned the latency half of the gate"
        );
    }

    #[test]
    fn the_error_rate_boundary_is_exclusive_of_the_one_in_a_thousand_bar() {
        // Exactly the README's bar (0.1% = 1/1000) must NOT pass: "under" is strict.
        assert!(
            !sustained_gate_passes(Some(1_000), 999, 1),
            "exactly 1/1000 failing is the boundary itself"
        );
        // One request short of the boundary sample (1 failure in 1001) is comfortably under 0.1% and
        // must pass.
        assert!(sustained_gate_passes(Some(1_000), 1_000, 1));
        // A single failure against a tiny sample is a large fraction and must fail.
        assert!(!sustained_gate_passes(Some(1_000), 9, 1));
    }

    // ── the streams-sustained gate: the README's own three-part rule ─────────────────────────────

    /// A window that passes everything, as a starting point each test below breaks in one way.
    fn clean_stream_window(concurrency: u32, streams: u64) -> StreamWindow {
        let budget = crate::metric::STREAM_FRAME_BUDGET as u64;
        // Shaped like a real openai window: the budget's worth of SSE events, one of which is the
        // dialect's role head, so the content counts are the budget minus that prelude. A helper
        // that set content == every event would model the very state RIG-11 was about.
        let content = budget - Dialect::Openai.stream_prelude_frames();
        StreamWindow {
            concurrency,
            streams,
            errored: 0,
            error_kinds: StreamErrorKinds::default(),
            host_before: HostState::default(),
            frames: streams * budget,
            expected_frames: streams * budget,
            content_frames: streams * content,
            expected_content_frames: streams * content,
            stalls: 0,
            elapsed_s: 1.0,
        }
    }

    #[test]
    fn a_window_that_delivered_every_frame_cleanly_holds_the_gate() {
        assert!(streams_gate_passes(&clean_stream_window(64, 64)));
    }

    // Every frame: a dropped frame is a dropped token, at any concurrency.
    #[test]
    fn a_single_lost_frame_fails_the_rung() {
        let mut w = clean_stream_window(1000, 1000);
        assert!(
            streams_gate_passes(&w),
            "a window that delivered everything must pass: {w:?}"
        );
        w.content_frames -= 1;
        assert!(
            !streams_gate_passes(&w),
            "one lost frame must fail the rung: {w:?}"
        );
        // The old 0.999 bar waved this through, and at a 64-frame budget that is 16 frames a window.
        w.content_frames = (w.expected_content_frames as f64 * 0.999).ceil() as u64;
        assert!(
            !streams_gate_passes(&w),
            "99.9% is still a gateway losing tokens: {w:?}"
        );
    }

    // Both stream searches judge a window the same way (the retired cpu-fps probe used a laxer
    // `errored == 0 && frames > 0` check that let a 1-of-64 window count as healthy).
    #[test]
    fn both_stream_searches_use_the_same_definition_of_a_healthy_window() {
        let mut w = clean_stream_window(64, 64);
        w.frames = 1;
        w.content_frames = 1;
        assert!(
            !streams_gate_passes(&w),
            "1 frame of 64 is not a healthy window: {w:?}"
        );
        assert!(
            w.errored == 0 && w.frames > 0,
            "...yet the old cpu-fps gate accepted exactly this"
        );
    }

    // "no stream stalls past 2x the pacing interval". ONE stall anywhere fails the rung: a stall is a
    // user-visible gap in a token stream, not a rate to be averaged away across lanes.
    #[test]
    fn a_single_stall_anywhere_fails_the_whole_rung() {
        let mut w = clean_stream_window(1000, 1000);
        w.stalls = 1;
        assert!(!streams_gate_passes(&w));
    }

    // "the stream error rate stays under 0.1%". Strictly under, exactly as the request-side
    // `sustained_gate_passes` reads its own identical bar.
    #[test]
    fn the_stream_error_rate_boundary_is_exclusive_of_the_one_in_a_thousand_bar() {
        let mut w = clean_stream_window(1000, 1000);
        w.errored = 1;
        assert!(
            !streams_gate_passes(&w),
            "exactly 1/1000 failing is the boundary itself"
        );
        let mut w = clean_stream_window(1001, 1001);
        w.errored = 1;
        assert!(
            streams_gate_passes(&w),
            "1 in 1001 is comfortably under the bar"
        );
    }

    // A ratio computed from zero must never read as a clean window by floating-point accident (same
    // trap `sustained_gate_passes` guards).
    #[test]
    fn a_window_that_opened_no_stream_never_reads_as_a_pass() {
        let mut w = clean_stream_window(4, 0);
        w.frames = 0;
        w.expected_frames = 0;
        w.content_frames = 0;
        w.expected_content_frames = 0;
        assert!(!streams_gate_passes(&w));
        assert_eq!(w.delivery_ratio(), 0.0);
        assert_eq!(w.error_ratio(), 1.0, "no streams is not a zero error rate");
    }

    // No elapsed time means no rate; an infinity would win every peak search it appeared in.
    #[test]
    fn a_window_with_no_elapsed_time_reports_no_rate_rather_than_an_infinity() {
        let mut w = clean_stream_window(4, 4);
        w.elapsed_s = 0.0;
        assert_eq!(w.fps(), 0.0);
        assert!(w.fps().is_finite());
    }

    // ── the stall bound itself ───────────────────────────────────────────────────────────────────

    // Gaps, not the first frame's offset: time-to-first-token is `Streaming`'s job, not a stall here.
    #[test]
    fn a_late_first_frame_is_not_a_stall_but_a_late_second_one_is() {
        let bound = stream_pacing_interval_ms() * STREAM_STALL_MULTIPLIER * 1_000;
        // One enormous offset, no gaps at all: nothing to stall on.
        assert_eq!(stalls_in(&[10 * bound]), 0);
        // Exactly the bound is not "past" it; one microsecond more is.
        assert_eq!(stalls_in(&[0, bound]), 0, "the bound itself is not a stall");
        assert_eq!(stalls_in(&[0, bound + 1]), 1);
        // The mock's own pace (20 ms between deltas) must never register.
        let paced: Vec<u64> = (0..64)
            .map(|i| i * stream_pacing_interval_ms() * 1_000)
            .collect();
        assert_eq!(stalls_in(&paced), 0, "the mock's own pacing is not a stall");
    }

    // ── a real window against a real SSE peer ───────────────────────────────────────────────────

    /// A minimal SSE peer shaped like the mock's openai stream: one role head frame (no content),
    /// then `frames` content deltas `gap_ms` apart, then close. The head frame matters: without it,
    /// a test can't distinguish RIG-11's fixed delivery ratio from the one it replaced.
    fn serve_sse(frames: usize, gap_ms: u64) -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                std::thread::spawn(move || {
                    let mut b = [0u8; 4096];
                    if c.read(&mut b).unwrap_or(0) == 0 {
                        return;
                    }
                    let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nConnection: close\r\n\r\n";
                    if c.write_all(head.as_bytes()).is_err() {
                        return;
                    }
                    // The role head: openai's first chunk, which carries no token.
                    let role = "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n";
                    if c.write_all(role.as_bytes()).is_err() {
                        return;
                    }
                    let _ = c.flush();
                    for i in 0..frames {
                        if gap_ms > 0 {
                            std::thread::sleep(Duration::from_millis(gap_ms));
                        }
                        let frame = format!(
                            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"t{i}\"}}}}]}}\n\n"
                        );
                        if c.write_all(frame.as_bytes()).is_err() {
                            return;
                        }
                        let _ = c.flush();
                    }
                });
            }
        });
        addr
    }

    #[test]
    fn a_stream_window_reads_every_lane_and_counts_its_frames() {
        let sse = serve_sse(crate::metric::STREAM_FRAME_BUDGET, 0);
        let w = stream_window(sse, "/v1/chat/completions", "{}", &[], Dialect::Openai, 4)
            .expect("four lanes against a live SSE peer must produce a window");
        assert_eq!(w.streams, 4, "every lane must be joined and counted: {w:?}");
        assert_eq!(
            w.errored, 0,
            "a well-framed event stream is not an error: {w:?}"
        );
        assert_eq!(w.frames, 4 * crate::metric::STREAM_FRAME_BUDGET as u64);
        assert_eq!(
            w.expected_frames, w.frames,
            "a full-budget peer leaves no shortfall"
        );
        // Not `stalls == 0`: under parallel test load, occasional stalls are a fact about the
        // machine, not this window. The gate assertion below is what actually matters.
        assert!(
            w.stalls <= w.frames,
            "a stall is a gap BETWEEN frames, so it cannot exceed them: {w:?}"
        );
        assert!(
            w.fps() > 0.0,
            "a window that read frames must carry a rate: {w:?}"
        );
        assert!(
            streams_gate_passes(&w),
            "a clean full-delivery window holds the gate: {w:?}"
        );
    }

    /// A peer that answers well-formed JSON rather than an event stream; declares its content-type
    /// so `post_json_sse` short-circuits instead of waiting out its deadline.
    fn serve_json(status: u16) -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                std::thread::spawn(move || {
                    let mut b = [0u8; 4096];
                    while c.read(&mut b).unwrap_or(0) > 0 {
                        let r = format!(
                            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: 2\r\n\r\nok"
                        );
                        if c.write_all(r.as_bytes()).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        addr
    }

    // A peer that answers plain JSON has not streamed: an errored stream, not a delivery shortfall.
    #[test]
    fn a_peer_that_answers_json_instead_of_frames_counts_as_an_errored_stream() {
        let json = serve_json(200);
        let w = stream_window(json, "/v1/chat/completions", "{}", &[], Dialect::Openai, 2)
            .expect("the window still ran");
        assert_eq!(w.streams, 2);
        assert_eq!(
            w.errored, 2,
            "a non-event-stream answer is a failed stream: {w:?}"
        );
        assert_eq!(w.frames, 0);
        assert!(!streams_gate_passes(&w));
    }

    // A stream that opens then delivers short is a delivery shortfall, not an error.
    #[test]
    fn a_short_stream_is_a_delivery_shortfall_rather_than_an_error() {
        let short = crate::metric::STREAM_FRAME_BUDGET / 2;
        let sse = serve_sse(short, 0);
        let w = stream_window(sse, "/v1/chat/completions", "{}", &[], Dialect::Openai, 2)
            .expect("the window ran");
        assert_eq!(
            w.errored, 0,
            "the stream existed; it just ended early: {w:?}"
        );
        // The role head arrives too, so the raw event count is one more per lane than the tokens.
        assert_eq!(w.frames, 2 * (short as u64 + 1));
        assert_eq!(w.content_frames, 2 * short as u64);
        assert!(w.delivery_ratio() < STREAM_MIN_DELIVERY_RATIO);
        assert!(!streams_gate_passes(&w));
    }

    // Clock starts after lanes exist; asserted at the clock site (see `started`), not via a timing
    // bound here, since a machine-speed-dependent constant was never a reliable proxy for ordering.
    #[test]
    fn a_stream_window_counts_every_lane_it_was_asked_for() {
        // A fleet, not a stampede: keep concurrency modest to avoid starving neighbouring tests.
        let concurrency = 64;
        let sse = serve_sse(crate::metric::STREAM_FRAME_BUDGET, 0);
        let w = stream_window(
            sse,
            "/v1/chat/completions",
            "{}",
            &[],
            Dialect::Openai,
            concurrency,
        )
        .expect("a fleet of instant-answering lanes must produce a window");
        assert_eq!(
            w.streams,
            u64::from(concurrency),
            "every lane must be joined and counted"
        );
        assert_eq!(w.errored, 0, "an instant peer errors no stream: {w:?}");
        assert_eq!(
            w.frames,
            u64::from(concurrency) * crate::metric::STREAM_FRAME_BUDGET as u64
        );
        assert!(
            w.elapsed_s > 0.0 && w.elapsed_s.is_finite(),
            "the window must report a real clock: {w:?}"
        );
        // fps() divides by that clock, so a zero or negative one would publish an infinity.
        assert!(w.fps() > 0.0 && w.fps().is_finite(), "{w:?}");
    }

    // The gateway leg's own frames/sec is meaningless without a reference, and the reference must be
    // absent rather than zero when the mock could not be reached: `is_rig_bound` reads an unusable
    // reference as "unknown", and a zero would read as "the gateway beat the rig".
    #[test]
    fn an_unreachable_mock_yields_an_absent_stream_reference_never_a_zero() {
        let dead: SocketAddr = "127.0.0.1:1".parse().expect("literal");
        let m = stream_fps_at(dead, "m", "dummy", Dialect::Openai, 2);
        assert_eq!(m.copied(), None);
        assert_eq!(m.reason(), Some(&Absent::NotMeasured));
    }

    #[test]
    fn a_window_with_ok_and_fail_both_zero_reads_as_all_failed_never_a_pass() {
        // Pins the function's own behaviour on 0/0 regardless of callers already filtering it: a
        // fail ratio computed as 0/0 must never read as clean by floating-point accident.
        assert!(!sustained_gate_passes(Some(1), 0, 0));
    }

    // A server that is busy now and fine in a moment must not be recorded as incapable. Uses a
    // shortened pause through the same budget the field uses, so the test exercises the real loop.
    fn serve_busy_then_ok(busy_times: usize) -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                let seen = seen.clone();
                std::thread::spawn(move || {
                    let mut b = [0u8; 4096];
                    while c.read(&mut b).unwrap_or(0) > 0 {
                        let n = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        let (status, body) = if n < busy_times {
                            (503, "{\"error\":{\"message\":\"The service is temporarily overloaded.\"}}")
                        } else {
                            (200, "{\"ok\":true}")
                        };
                        let r = format!(
                            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                            body.len()
                        );
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
    fn a_gateway_that_answers_503_once_is_retried_not_recorded_as_not_serving() {
        let addr = serve_busy_then_ok(1);
        let cfg = test_fixture(addr, addr);
        let served = probe_cell_within(
            &cfg,
            &CellId::new("openai", "openai"),
            true,
            3,
            Duration::from_millis(10),
        );
        assert_eq!(
            served,
            Served::Yes,
            "one 503 became a permanent capability verdict; this is the 26 busbar cells that \
             published as red while every one of its lanes was healthy"
        );
    }

    // The budget is bounded, and a gateway that is genuinely down stays down. Exhausting it must
    // still produce the real verdict rather than an optimistic one.
    #[test]
    fn a_gateway_that_stays_503_across_the_whole_budget_is_still_recorded_as_failed() {
        let addr = serve_busy_then_ok(usize::MAX);
        let cfg = test_fixture(addr, addr);
        match probe_cell_within(
            &cfg,
            &CellId::new("openai", "openai"),
            true,
            3,
            Duration::from_millis(10),
        ) {
            Served::No(crate::probe::Verdict::Failed, ev) => assert_eq!(ev.status, 503),
            other => panic!("a persistently-503 gateway must still fail, got {other:?}"),
        }
    }

    // A gateway that does not answer in time is a moment, not a capability — the transport-failure
    // analogue of the transient-status retry above.
    fn serve_silent_then_ok(silent_times: usize) -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        let seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                let seen = seen.clone();
                std::thread::spawn(move || {
                    let mut b = [0u8; 4096];
                    while c.read(&mut b).unwrap_or(0) > 0 {
                        let n = seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if n < silent_times {
                            // Accept, then say nothing: exactly the shape that reads as
                            // "accepted the connection and never answered".
                            std::thread::sleep(std::time::Duration::from_secs(30));
                            return;
                        }
                        let body = "{\"ok\":true}";
                        let r = format!(
                            "HTTP/1.1 200 X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                            body.len()
                        );
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
    fn a_gateway_that_does_not_answer_once_is_retried_not_written_off() {
        let addr = serve_silent_then_ok(1);
        let cfg = test_fixture(addr, addr);
        assert_eq!(
            probe_cell_within(
                &cfg,
                &CellId::new("openai", "openai"),
                true,
                3,
                Duration::from_millis(10)
            ),
            Served::Yes,
            "one timeout cost this cell entirely; these are litellm-python's three lost cells"
        );
    }

    // The budget is bounded on this door too: a gateway that never answers still ends up untestable
    // rather than being retried forever or, worse, optimistically passed.
    #[test]
    fn a_gateway_that_never_answers_is_still_untestable_after_the_budget() {
        let addr = serve_silent_then_ok(usize::MAX);
        let cfg = test_fixture(addr, addr);
        match probe_cell_within(
            &cfg,
            &CellId::new("openai", "openai"),
            true,
            3,
            Duration::from_millis(10),
        ) {
            Served::Untestable(why) => assert!(why.contains("never answered"), "got {why}"),
            other => panic!("a silent gateway must stay untestable, got {other:?}"),
        }
    }

    // The stall bound follows the mock's actual pace (read from its env var), not a hardcoded copy
    // of its default, so changing the mock's pacing can't silently desync the two.
    #[test]
    fn the_stall_bound_tracks_the_pace_the_mock_was_actually_told_to_use() {
        // Default: the mock's own default, so the field behaviour is unchanged.
        std::env::remove_var("MOCK_STREAM_INTERVAL_MS");
        assert_eq!(stream_pacing_interval_ms(), 20);
        assert_eq!(stall_bound_us(), 20 * STREAM_STALL_MULTIPLIER * 1_000);

        // A slower model: gaps that were stalls at 20ms are ordinary at 100ms, and a bound that
        // stayed at 20 would call a healthy stream stalled on every frame.
        std::env::set_var("MOCK_STREAM_INTERVAL_MS", "100");
        assert_eq!(stall_bound_us(), 100 * STREAM_STALL_MULTIPLIER * 1_000);
        let gaps_at_100ms: Vec<u64> = (0..8).map(|i| i * 100_000).collect();
        assert_eq!(
            stalls_in(&gaps_at_100ms),
            0,
            "a stream keeping the mock's own pace never stalls"
        );

        // A faster model: the bound tightens with it, so a gateway that lags is still caught.
        std::env::set_var("MOCK_STREAM_INTERVAL_MS", "5");
        assert_eq!(stall_bound_us(), 5 * STREAM_STALL_MULTIPLIER * 1_000);
        assert!(
            stalls_in(&gaps_at_100ms) > 0,
            "at a 5ms pace those same 100ms gaps are stalls"
        );

        // Garbage is not a pace: fall back to the mock's default rather than to zero, which would
        // make every gap a stall.
        std::env::set_var("MOCK_STREAM_INTERVAL_MS", "not-a-number");
        assert_eq!(stream_pacing_interval_ms(), 20);
        std::env::remove_var("MOCK_STREAM_INTERVAL_MS");
    }

    // The ceiling is the host's own port range, derived rather than a chosen constant.
    #[test]
    fn the_connection_ceiling_comes_from_the_hosts_port_range_not_a_chosen_constant() {
        // The derivation, over the ranges that matter: stock Linux, the range the orchestrator sets,
        // and the widest a host can offer. Powers of two because the ladder doubles - a ceiling
        // between rungs is reachable only by the clamp and makes the top rung a different shape.
        let ceiling_for = |lo: u32, hi: u32| {
            let usable = hi - lo + 1;
            let mut c = 1u32;
            while c * 2 <= usable {
                c *= 2;
            }
            c
        };
        assert_eq!(
            ceiling_for(32_768, 60_999),
            16_384,
            "stock Linux: ~28k ports"
        );
        assert_eq!(
            ceiling_for(16_384, 65_535),
            32_768,
            "what run-on-ec2.sh sets"
        );
        assert_eq!(
            ceiling_for(1_024, 65_535),
            32_768,
            "the widest a host can give"
        );

        // The real function agrees with that derivation on whatever host runs the test, and never
        // returns something the ladder cannot climb to.
        let c = host_connection_ceiling();
        assert!(
            c.is_power_of_two(),
            "the ceiling must be a rung the ladder actually lands on, got {c}"
        );
        assert!(
            c >= 1024,
            "a ceiling below the concurrencies this field routinely reaches is not usable, got {c}"
        );
        // On Linux it must match the host's real range; off Linux it falls back to the stock range's
        // own derivation rather than to a number someone chose.
        if let Ok(t) = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range") {
            let mut p = t.split_whitespace().filter_map(|v| v.parse::<u32>().ok());
            if let (Some(lo), Some(hi)) = (p.next(), p.next()) {
                assert_eq!(
                    c,
                    ceiling_for(lo, hi),
                    "the ceiling must be this host's own range"
                );
            }
        } else {
            assert_eq!(
                c,
                ceiling_for(32_768, 60_999),
                "off Linux, assume a stock host"
            );
        }
    }

    // The rig running out of ports is not an errored stream: the gateway was never asked.
    #[test]
    fn a_lane_this_host_could_not_open_is_not_the_gateway_erroring() {
        let ours = crate::http::SseOutcome {
            status: None,
            frames: Vec::new(),
            frame_offsets_us: Vec::new(),
            content_frames: 0,
            end: crate::http::SseEnd::RigExhausted(
                "Cannot assign requested address (os error 99)".into(),
            ),
        };
        assert!(
            !stream_errored(&ours),
            "our own port exhaustion must not be charged to the gateway"
        );

        // The peer refusing IS the gateway's, and must still count.
        let theirs = crate::http::SseOutcome {
            status: None,
            frames: Vec::new(),
            frame_offsets_us: Vec::new(),
            content_frames: 0,
            end: crate::http::SseEnd::ConnectionFailed("Connection refused (os error 111)".into()),
        };
        assert!(
            stream_errored(&theirs),
            "a refused connection is still the gateway declining"
        );

        // So is a peer that answers something that is not a stream.
        let not_sse = crate::http::SseOutcome {
            status: Some(200),
            frames: Vec::new(),
            frame_offsets_us: Vec::new(),
            content_frames: 0,
            end: crate::http::SseEnd::NotAnEventStream("application/json".into()),
        };
        assert!(stream_errored(&not_sse));
    }

    // THE RIG MUST BE RE-CONFIRMED AS THE GRID RUNS, not once at the start.
    //
    // `mock_healthy` is the input that lets a verdict come back NotVerified: when the rig cannot
    // vouch for itself, nothing observed is attributable to the gateway. Reading it once before a
    // grid that runs for ninety minutes defeated that - a mock that degraded partway left every
    // later cell graded as though the rig were fine, turning our failure into the gateway's verdict.
    //
    // Asserted on the verdict rule rather than by driving a whole grid, because the rule is where
    // the consequence lives and a grid test cannot make a mock die halfway on demand.
    #[test]
    fn an_unconfirmed_rig_makes_the_same_observation_unattributable() {
        use crate::probe::{persistent_transient_verdict, Observation, Verdict};

        // The identical status, seen with the rig confirmed and unconfirmed, must not produce the
        // same verdict - that is the whole reason the flag is threaded down here.
        for status in [500u16, 503, 400, 404] {
            let confirmed = persistent_transient_verdict(Observation {
                status: Some(status),
                mock_healthy: true,
            });
            let unconfirmed = persistent_transient_verdict(Observation {
                status: Some(status),
                mock_healthy: false,
            });
            assert_ne!(
                confirmed, unconfirmed,
                "HTTP {status} must not read the same when the rig could not confirm itself"
            );
            assert_eq!(
                unconfirmed,
                Verdict::NotVerified,
                "an unconfirmed rig makes HTTP {status} unattributable"
            );
        }

        // And with the rig confirmed, the gateway's own answers still classify normally.
        assert_eq!(
            persistent_transient_verdict(Observation {
                status: Some(404),
                mock_healthy: true
            }),
            Verdict::NotConfigured
        );
        assert_eq!(
            persistent_transient_verdict(Observation {
                status: Some(503),
                mock_healthy: true
            }),
            Verdict::Failed
        );
    }

    // A ceiling the gateway holds one time in three is not a ceiling it sustains: the bisection
    // lands exactly on the boundary rung, which can be marginal, hence the majority confirmation.
    #[test]
    fn a_ceiling_is_confirmed_by_a_majority_of_its_own_windows() {
        // The rule the confirmation applies, with the bisection's own winning window counted as one
        // vote - it is a real measurement at that concurrency and discarding it throws away evidence.
        let holds = |repeats: &[bool]| {
            let held = 1 + repeats.iter().filter(|ok| **ok).count();
            let total = 1 + repeats.len();
            held * 2 > total
        };

        // Bisection window passed, both confirmations failed: 1 of 3 is not a ceiling.
        assert!(
            !holds(&[false, false]),
            "1 of 3 windows must not confirm a ceiling"
        );
        // A genuine ceiling: the confirmations agree with the bisection.
        assert!(holds(&[true, true]), "3 of 3 must confirm");
        // The marginal case - 2 of 3 - is a majority and confirms. A gateway that holds two thirds of
        // the time is sustaining it; demanding unanimity would reject real ceilings for one unlucky
        // window, which is the opposite error.
        assert!(
            holds(&[true, false]),
            "2 of 3 is a majority and must confirm"
        );

        // Stepping down halves the concurrency, so the walk is bounded and always moves toward a
        // region already known to pass.
        let mut c = 252u32;
        let mut steps = 0;
        while steps < MAX_CEILING_STEPDOWNS {
            let next = c / 2;
            assert!(next < c, "a step-down must make progress");
            c = next;
            steps += 1;
        }
        assert!(
            c < 252 / 8,
            "four halvings must reach well below the failing ceiling, got {c}"
        );
    }

    // The sustained ceiling's own confirmation is driven end to end by
    // `every_published_ceiling_was_measured_by_a_full_rung_of_windows`, against a scripted probe.
    // The stream ceiling's confirmation (`sweep_streams_cell`) runs the same majority rule inline
    // and is not reachable without a live SSE gateway, so it stays covered by the end-to-end run
    // rather than by a unit test asserting against this file's own text.

    // ── the rig refuses to smuggle: the lanes run.rs owns ───────────────────────────────────────

    // Ledger RIG-12: a probe we refuse to send is `Served::Untestable`, never a capability verdict —
    // `Served::No` here would convict a gateway of our own manifest defect.
    #[test]
    fn a_probe_the_rig_refuses_to_send_is_untestable_rather_than_a_refusal() {
        // A live, healthy peer, so nothing about this verdict can come from an address with nothing
        // on it: the refusal happens before the connect, and that is the point.
        let peer = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = peer.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in peer.incoming() {
                let Ok(mut c) = c else { continue };
                let mut b = [0u8; 4096];
                let _ = c.read(&mut b);
                let _ = c.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}");
            }
        });

        let mut cfg = test_fixture(addr, addr);
        cfg.static_headers = vec![("x-route".into(), "a\r\nx-injected: b".into())];
        let id = CellId::new("openai", "openai");
        let (served, retry) = probe_cell_once(&cfg, &id, true);
        match &served {
            Served::Untestable(why) => assert!(
                why.contains("never asked") || why.contains("refused to send"),
                "the verdict must say the gateway was never asked: {why}"
            ),
            other => panic!("a request we refused to send was graded as {other:?}"),
        }
        assert!(
            retry.is_none(),
            "asking again spends the budget to be refused identically"
        );

        // The same cfg without the hostile header reaches the peer, so this test proves a refusal
        // rather than a broken fixture.
        cfg.static_headers = vec![("x-route".into(), "b".into())];
        assert!(
            !matches!(probe_cell_once(&cfg, &id, true).0, Served::Untestable(_)),
            "a benign header must still be probed"
        );
    }

    // A stream window we refuse to send is unmeasured, not a failing rung or an errored stream.
    #[test]
    fn a_stream_window_the_rig_refuses_to_send_is_unmeasured_not_a_failing_rung() {
        let sse = serve_sse(crate::metric::STREAM_FRAME_BUDGET, 0);
        let hostile = vec![("authorization".to_string(), "Bearer t\r\nx: y".to_string())];
        assert!(
            stream_window(
                sse,
                "/v1/chat/completions",
                "{}",
                &hostile,
                Dialect::Openai,
                4
            )
            .is_none(),
            "a window whose request we would not send has measured nothing about the gateway"
        );
        // And the lane-level rule underneath it: a refusal is not the gateway erroring.
        let refused = crate::http::SseOutcome {
            status: None,
            frames: Vec::new(),
            frame_offsets_us: Vec::new(),
            content_frames: 0,
            end: crate::http::SseEnd::RigRefused("a header we will not send".into()),
        };
        assert!(
            !stream_errored(&refused),
            "our own refusal must not be charged to the gateway's stream error rate"
        );
    }

    // ── delivery counts TOKENS, not events (ledger RIG-11) ──────────────────────────────────────

    /// A peer that answers with a full budget of well-formed openai events, none of which carries a
    /// token. The old event-count accounting couldn't tell this apart from a perfect stream.
    fn serve_sse_without_tokens(events: usize) -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                std::thread::spawn(move || {
                    let mut b = [0u8; 4096];
                    if c.read(&mut b).unwrap_or(0) == 0 {
                        return;
                    }
                    let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nConnection: close\r\n\r\n";
                    if c.write_all(head.as_bytes()).is_err() {
                        return;
                    }
                    for _ in 0..events {
                        let frame = "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":null}]}\n\n";
                        if c.write_all(frame.as_bytes()).is_err() {
                            return;
                        }
                        let _ = c.flush();
                    }
                });
            }
        });
        addr
    }

    // Ledger RIG-11: a stream that delivered no tokens has not delivered, even if it filled its
    // whole frame budget with framing scaffolding.
    #[test]
    fn a_full_budget_of_scaffolding_is_not_a_delivered_stream() {
        let peer = serve_sse_without_tokens(crate::metric::STREAM_FRAME_BUDGET);
        let w = stream_window(peer, "/v1/chat/completions", "{}", &[], Dialect::Openai, 2)
            .expect("the window ran");
        assert_eq!(
            w.frames,
            2 * crate::metric::STREAM_FRAME_BUDGET as u64,
            "every event still counts in `frames`, which is what fps is for: {w:?}"
        );
        assert_eq!(
            w.errored, 0,
            "the stream existed and was well-formed: {w:?}"
        );
        assert_eq!(w.content_frames, 0, "not one token arrived: {w:?}");
        assert_eq!(w.delivery_ratio(), 0.0);
        assert!(
            !streams_gate_passes(&w),
            "a stream that delivered no tokens must not hold the delivery gate: {w:?}"
        );
        let why = streams_gate_verdict(&w).expect("a failing rung publishes a reason");
        assert!(
            why.contains("content frames"),
            "the reason must say what was counted: {why}"
        );
    }

    // The other half: a peer that DOES deliver every token it could still holds the gate, so the
    // tightened numerator is not simply failing everything. The denominator is the budget minus the
    // dialect's prelude, which is why an openai stream delivering 63 tokens of a 64-frame budget is
    // complete rather than one short.
    #[test]
    fn a_stream_that_delivers_every_token_the_budget_allows_still_holds_the_gate() {
        let sse = serve_sse(crate::metric::STREAM_FRAME_BUDGET, 0);
        let w = stream_window(sse, "/v1/chat/completions", "{}", &[], Dialect::Openai, 2)
            .expect("the window ran");
        let budget = crate::metric::STREAM_FRAME_BUDGET as u64;
        assert_eq!(w.frames, 2 * budget);
        assert_eq!(
            w.expected_content_frames,
            2 * (budget - Dialect::Openai.stream_prelude_frames())
        );
        assert_eq!(w.content_frames, w.expected_content_frames, "{w:?}");
        assert_eq!(w.delivery_ratio(), 1.0);
        assert!(streams_gate_passes(&w), "{w:?}");
    }

    // ── the delivery budget is counted in CONTENT, not in events ────────────────────────────────

    /// An SSE peer shaped like a gateway rather than the mock: role head, an extra framing event the
    /// mock never sends, then content tokens with a keepalive between pairs. Every token is
    /// delivered; it just costs more events than the mock's layout (real behaviour: anthropic sends
    /// `ping`s, translation cells re-frame, keepalives insert events).
    fn serve_sse_with_gateway_framing(content: usize) -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                std::thread::spawn(move || {
                    let mut b = [0u8; 4096];
                    if c.read(&mut b).unwrap_or(0) == 0 {
                        return;
                    }
                    let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nConnection: close\r\n\r\n";
                    if c.write_all(head.as_bytes()).is_err() {
                        return;
                    }
                    // Two prelude events where the mock sends one.
                    let role = "data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n";
                    let ping = "data: {\"choices\":[{\"index\":0,\"delta\":{}}]}\n\n";
                    for f in [role, ping] {
                        if c.write_all(f.as_bytes()).is_err() {
                            return;
                        }
                    }
                    for i in 0..content {
                        let frame = format!(
                            "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"t{i}\"}}}}]}}\n\n"
                        );
                        if c.write_all(frame.as_bytes()).is_err() {
                            return;
                        }
                        if c.write_all(ping.as_bytes()).is_err() {
                            return;
                        }
                        let _ = c.flush();
                    }
                });
            }
        });
        addr
    }

    // A GATEWAY'S OWN FRAMING IS NOT A DELIVERY SHORTFALL.
    //
    // Denominator is `STREAM_FRAME_BUDGET - stream_prelude_frames()` (mock's layout); numerator is
    // measured on the gateway's stream. Reading to a fixed event count would let every extra framing
    // event the gateway emits displace a content frame and fail every rung for no real reason.
    #[test]
    fn a_gateway_that_spends_extra_events_on_framing_still_delivers_every_token() {
        let gw = serve_sse_with_gateway_framing(crate::metric::STREAM_FRAME_BUDGET);
        let w = stream_window(gw, "/v1/chat/completions", "{}", &[], Dialect::Openai, 2)
            .expect("the window ran");
        assert_eq!(
            w.errored, 0,
            "a well-framed event stream is not an error: {w:?}"
        );
        assert_eq!(
            w.content_frames, w.expected_content_frames,
            "every token the budget asks for arrived: {w:?}"
        );
        assert_eq!(w.delivery_ratio(), 1.0, "{w:?}");
        assert!(
            streams_gate_passes(&w),
            "a gateway that inserts framing and loses nothing must hold the gate: {:?}",
            streams_gate_verdict(&w)
        );
        // And it paid for that framing in EVENTS, which is the whole point: `frames` counts every
        // event and so runs past the mock-shaped `expected_frames`, while delivery is judged on the
        // content pair beside it.
        assert!(
            w.frames > w.expected_frames,
            "the extra framing must be visible in the raw event count: {w:?}"
        );
    }

    // The other side of the same read, so the fix is not simply a looser gate: a gateway with the
    // same extra framing that delivers one token FEWER than the budget asks for still fails. The
    // read waits for content rather than for events, so a token that never arrives is the only
    // reason the count can come up short.
    #[test]
    fn a_gateway_that_drops_a_token_fails_the_gate_however_it_frames_the_stream() {
        let content = crate::metric::STREAM_FRAME_BUDGET
            - Dialect::Openai.stream_prelude_frames() as usize
            - 1;
        let gw = serve_sse_with_gateway_framing(content);
        let w = stream_window(gw, "/v1/chat/completions", "{}", &[], Dialect::Openai, 2)
            .expect("the window ran");
        assert_eq!(
            w.errored, 0,
            "the stream existed and was well-framed; it lost a token: {w:?}"
        );
        assert_eq!(w.content_frames, 2 * content as u64, "{w:?}");
        assert!(w.delivery_ratio() < STREAM_MIN_DELIVERY_RATIO, "{w:?}");
        assert!(!streams_gate_passes(&w), "{w:?}");
        let why = streams_gate_verdict(&w).expect("a failing rung publishes a reason");
        assert!(
            why.contains("content frames"),
            "the reason must name what came up short: {why}"
        );
    }

    /// A peer that frames forever and never sends a token: the pathological case a content-budgeted
    /// read must be bounded against.
    fn serve_sse_endless_framing() -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                std::thread::spawn(move || {
                    let mut b = [0u8; 4096];
                    if c.read(&mut b).unwrap_or(0) == 0 {
                        return;
                    }
                    let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nConnection: close\r\n\r\n";
                    if c.write_all(head.as_bytes()).is_err() {
                        return;
                    }
                    let ping = "data: {\"choices\":[{\"index\":0,\"delta\":{}}]}\n\n";
                    // Until the reader hangs up, which is what the ceiling makes it do.
                    while c.write_all(ping.as_bytes()).is_ok() {
                        let _ = c.flush();
                    }
                });
            }
        });
        addr
    }

    // The read stays bounded by `STREAM_EVENT_CEILING` without waiting out `STREAM_TIMEOUT`, and
    // hitting the ceiling with no tokens is still a real delivery shortfall.
    #[test]
    fn an_endless_framing_stream_stops_at_the_event_ceiling_and_still_fails_the_gate() {
        let peer = serve_sse_endless_framing();
        let started = std::time::Instant::now();
        let w = stream_window(peer, "/v1/chat/completions", "{}", &[], Dialect::Openai, 2)
            .expect("the window ran");
        assert!(
            started.elapsed() < crate::metric::STREAM_TIMEOUT,
            "the ceiling, not the deadline, must be what ends this read: {w:?}"
        );
        assert_eq!(
            w.frames,
            2 * crate::metric::STREAM_EVENT_CEILING as u64,
            "each lane reads exactly to the ceiling: {w:?}"
        );
        assert_eq!(w.content_frames, 0, "not one token arrived: {w:?}");
        assert_eq!(
            w.errored, 0,
            "the peer answered 200 and framed correctly - it delivered nothing, which is the \
             delivery clause's finding rather than an errored stream: {w:?}"
        );
        assert!(!streams_gate_passes(&w), "{w:?}");
    }

    // ── one credential header on the wire (ledger RIG-12 remainder) ─────────────────────────────

    // Two `authorization` headers on one request is a measurement nobody can attribute (HTTP does
    // not define which same-named header a server honours).
    #[test]
    fn only_one_copy_of_a_header_the_rig_owns_reaches_the_wire() {
        let a: SocketAddr = "127.0.0.1:1".parse().expect("addr");
        let mut cfg = test_fixture(a, a);
        // Spelled with a capital A, matching the live manifest, to prove the dedup is case-insensitive.
        cfg.static_headers = vec![
            ("Authorization".into(), "Bearer manifest-token".into()),
            ("x-route".into(), "keep-me".into()),
        ];
        cfg.egress_headers = [(
            "openai".to_string(),
            vec![("authorization".to_string(), "Bearer per-column".to_string())],
        )]
        .into_iter()
        .collect();

        let h = headers_for(&cfg, Dialect::Openai, "openai");
        let auths: Vec<&(String, String)> = h
            .iter()
            .filter(|(n, _)| n.eq_ignore_ascii_case("authorization"))
            .collect();
        assert_eq!(
            auths.len(),
            1,
            "exactly one credential header may leave: {h:?}"
        );
        assert_eq!(
            auths[0].1, "Bearer dummy",
            "the surviving one is the rig's own, built from the token this run holds: {h:?}"
        );
        // Nothing else is dropped: a routing header selects a column's upstream.
        assert!(
            h.contains(&("x-route".to_string(), "keep-me".to_string())),
            "{h:?}"
        );

        // The dialect decides which names it owns, so anthropic's protocol constant is protected too.
        cfg.static_headers = vec![
            ("anthropic-version".into(), "1999-01-01".into()),
            ("X-Api-Key".into(), "manifest-key".into()),
        ];
        cfg.egress_headers = Default::default();
        let h = headers_for(&cfg, Dialect::Anthropic, "anthropic");
        assert_eq!(
            h.iter()
                .filter(|(n, _)| n.eq_ignore_ascii_case("anthropic-version"))
                .count(),
            1,
            "{h:?}"
        );
        assert_eq!(
            h.iter()
                .filter(|(n, _)| n.eq_ignore_ascii_case("x-api-key"))
                .count(),
            1,
            "{h:?}"
        );
        assert!(
            h.contains(&("x-api-key".to_string(), "dummy".to_string())),
            "{h:?}"
        );
        assert!(
            h.contains(&("anthropic-version".to_string(), "2023-06-01".to_string())),
            "{h:?}"
        );
    }

    // ── a rung with no reading has no failure count ─────────────────────────────────────────────

    // ── the derived mock ceiling, and the engine-fault check that replaced the old suppression ──────

    // Arithmetic, not a measurement: the mock sleeps `interval` before every delta except the first,
    // so this states the identity rather than a remembered number.
    #[test]
    fn the_mock_ceiling_is_the_mocks_own_declared_pacing_and_nothing_else() {
        let chunks = f64::from(mock_stream_chunks());
        let interval_s = stream_pacing_interval_ms() as f64 / 1000.0;
        for c in [1u32, 8, 256, 1024, 16_384] {
            let want = f64::from(c) * chunks / ((chunks - 1.0) * interval_s);
            let got = mock_frame_ceiling_fps(c);
            assert!(
                (got - want).abs() < 1e-6,
                "c={c}: {got} is not {chunks} frames per stream over {interval_s}s gaps"
            );
        }
        // Linear in concurrency, because the mock's pacing is per stream and the streams are concurrent.
        assert!(
            (mock_frame_ceiling_fps(2048) - 2.0 * mock_frame_ceiling_fps(1024)).abs() < 1e-6,
            "twice the streams is twice the frames at the same pace"
        );
    }

    // Pins the number this box actually has, so a defaults change can't quietly move the ceiling.
    #[test]
    fn the_ceiling_reproduces_the_bench_boxs_own_measured_night() {
        // Only meaningful on the box's defaults; if either knob is set, the identity above is the test.
        if mock_stream_chunks() != 64 || stream_pacing_interval_ms() != 20 {
            return;
        }
        let ceiling = mock_frame_ceiling_fps(1024);
        assert!(
            (ceiling - 52_012.7).abs() < 1.0,
            "1024 x 64 frames over 1.26s is 52013 frames/sec, got {ceiling}"
        );
        for (leg, observed, want_share) in [
            ("direct control", 25_893.0),
            ("through the gateway", 43_297.0),
        ]
        .iter()
        .zip([0.4978, 0.8325])
        .map(|((l, o), w)| (*l, *o, w))
        {
            let share = observed / ceiling;
            assert!(
                (share - want_share).abs() < 0.001,
                "{leg} read {observed}, which is {share} of physics"
            );
            assert!(
                share < 1.0,
                "{leg} did not exceed the mock's own pacing, so nothing that night was impossible"
            );
        }
    }

    // A ceiling of zero, not an infinity, when there is nothing to bound.
    #[test]
    fn no_streams_means_no_frame_rate_to_bound_against() {
        assert_eq!(mock_frame_ceiling_fps(0), 0.0);
    }

    // ── engine_fault: exact, and only where an exact bound exists ───────────────────────────────────

    // A window at its budget is the success case and must never be called a fault.
    #[test]
    fn a_window_delivering_exactly_its_content_budget_is_not_a_fault() {
        let w = StreamWindow {
            concurrency: 1024,
            streams: 1024,
            errored: 0,
            error_kinds: StreamErrorKinds::default(),
            host_before: HostState::default(),
            frames: 65_536,
            expected_frames: 65_536,
            content_frames: 64_512,
            expected_content_frames: 64_512,
            stalls: 0,
            elapsed_s: 1.26,
        };
        assert_eq!(w.engine_fault(), None, "{w:?}");
    }

    // More model output than the mock could have sent is our bug — no gateway behaviour produces
    // this. Exact, no tolerance.
    #[test]
    fn counting_more_content_than_the_mock_can_send_is_this_engines_fault() {
        let w = StreamWindow {
            concurrency: 1024,
            streams: 1024,
            errored: 0,
            error_kinds: StreamErrorKinds::default(),
            host_before: HostState::default(),
            frames: 65_536,
            expected_frames: 65_536,
            content_frames: 64_513,
            expected_content_frames: 64_512,
            stalls: 0,
            elapsed_s: 1.26,
        };
        let why = w.engine_fault().expect("one frame over budget is a fault");
        assert!(why.contains("64513") && why.contains("64512"), "{why}");
        assert!(
            why.contains("counted wrong"),
            "it must read as OUR defect, not as a finding about the gateway: {why}"
        );
    }

    // Ledger RIG-11: extra SSE events (pings, re-framing) are legal and must not be a fault.
    #[test]
    fn a_gateway_that_adds_its_own_framing_is_not_a_fault() {
        let w = StreamWindow {
            concurrency: 256,
            streams: 256,
            errored: 0,
            error_kinds: StreamErrorKinds::default(),
            host_before: HostState::default(),
            // Well over the mock's own event layout: pings between every delta.
            frames: 40_000,
            expected_frames: 16_384,
            // Content exactly at budget - not one token more.
            content_frames: 16_128,
            expected_content_frames: 16_128,
            stalls: 0,
            elapsed_s: 1.26,
        };
        assert_eq!(
            w.engine_fault(),
            None,
            "extra framing is the gateway's style, not our miscount: {w:?}"
        );
    }

    // A non-finite rate means the clock or counter is broken.
    #[test]
    fn a_rate_that_is_not_finite_is_this_engines_fault() {
        let w = StreamWindow {
            concurrency: 8,
            streams: 8,
            errored: 0,
            error_kinds: StreamErrorKinds::default(),
            host_before: HostState::default(),
            frames: 512,
            expected_frames: 512,
            content_frames: 504,
            expected_content_frames: 504,
            stalls: 0,
            elapsed_s: f64::NAN,
        };
        let why = w.engine_fault().expect("a NaN clock is a fault");
        assert!(why.contains("is wrong"), "{why}");
    }
}

#[cfg(test)]
mod stream_stop_tests {
    use super::*;

    // The five stream-search endings must say five different things, and the two that are ours
    // must be attributed to the harness.
    #[test]
    fn each_way_the_stream_search_ends_reports_its_own_cause() {
        let proved = 3144;
        let budget = MAX_CEILING_STEPDOWNS;
        let cases = [
            StreamStop::RigRanShort {
                measured: 1,
                wanted: 3,
            },
            StreamStop::FloorReached { last: 4 },
            StreamStop::SteppedRungFailed { at: 1572 },
            StreamStop::WindowUnavailable { at: 786 },
            StreamStop::BudgetExhausted,
        ];
        let texts: Vec<String> = cases.iter().map(|c| c.describe(proved, budget)).collect();
        for (i, a) in texts.iter().enumerate() {
            for (j, b) in texts.iter().enumerate() {
                assert!(
                    i == j || a != b,
                    "two different endings publish the SAME sentence, which is the defect: {a}"
                );
            }
            assert!(
                a.contains("3144"),
                "every reason names the concurrency the bisection proved"
            );
        }

        // The attribution is the half that matters: a window the rig failed to take must not be
        // filed under NotMeasured (which is a statement about the gateway).
        assert!(
            matches!(
                StreamStop::RigRanShort {
                    measured: 1,
                    wanted: 3
                }
                .absent_kind(),
                Absent::HarnessError
            ),
            "a rig shortfall must be a HarnessError, not a gateway measurement"
        );
        assert!(
            matches!(
                StreamStop::WindowUnavailable { at: 1 }.absent_kind(),
                Absent::HarnessError
            ),
            "a window we could not take is our failure"
        );
        assert!(
            matches!(
                StreamStop::BudgetExhausted.absent_kind(),
                Absent::NotMeasured
            ),
            "exhausting the step-down budget IS a statement about the gateway"
        );
        assert!(
            matches!(
                StreamStop::SteppedRungFailed { at: 1 }.absent_kind(),
                Absent::NotMeasured
            ),
            "a rung that failed the gate is the gateway's result"
        );

        // And the rig case must SAY it was the rig, in words a reader of the board will see.
        let rig = StreamStop::RigRanShort {
            measured: 1,
            wanted: 3,
        }
        .describe(proved, budget);
        assert!(
            rig.contains("RIG ran short") && rig.contains("not the gateway"),
            "the rig's own shortfall must name itself rather than reading as the gateway's failure: {rig}"
        );
    }
}

#[cfg(test)]
mod contamination_tests {
    use super::*;

    /// The guard must not fire where draining cannot explain anything, or it becomes a universal
    /// excuse relabelling real gateway failures as rig faults.
    #[test]
    fn the_contamination_guard_only_applies_where_draining_could_explain_it() {
        let fires = |proven: u32, at: u32| proven > STREAM_SETTLE_FREE_BELOW && at <= proven;
        assert!(fires(4096, 3088), "busbar's real case must be caught");
        assert!(
            !fires(2, 1),
            "a rung failing at c=1 under c=2 has no residue to blame"
        );
        assert!(
            !fires(4096, 5000),
            "a failure ABOVE the proven rung is ordinary evidence"
        );
        assert!(
            !fires(STREAM_SETTLE_FREE_BELOW, 8),
            "at or below the settle threshold, no excuse"
        );
    }

    // A rung failing below one the same cell already passed is ours, and must be filed as ours:
    // `Absent::NotMeasured` would put the finding among the gateway's results instead.
    #[test]
    fn a_contaminated_rung_is_a_harness_error_and_never_the_gateways_result() {
        assert_eq!(
            StreamStop::RigContaminated {
                at: 3088,
                proven: 4096
            }
            .absent_kind(),
            Absent::HarnessError,
            "a rung failing below a proven-clean one is the rig; filing it as NotMeasured charges \
             the gateway for our own undrained host"
        );
        // The endings that ARE about the gateway must stay that way, or this variant has just
        // laundered every real failure into a rig excuse.
        assert_eq!(
            StreamStop::SteppedRungFailed { at: 100 }.absent_kind(),
            Absent::NotMeasured
        );
        assert_eq!(
            StreamStop::BudgetExhausted.absent_kind(),
            Absent::NotMeasured
        );
        assert_eq!(
            StreamStop::FloorReached { last: 8 }.absent_kind(),
            Absent::NotMeasured
        );
    }

    /// Must carry both concurrencies: either number alone reads like an ordinary failure.
    #[test]
    fn the_contaminated_reason_names_the_rung_and_what_the_cell_had_already_carried() {
        let d = StreamStop::RigContaminated {
            at: 3088,
            proven: 4096,
        }
        .describe(6176, 6);
        assert!(d.contains("c=3088"), "must name the rung that failed: {d}");
        assert!(
            d.contains("c=4096"),
            "must name what this cell already carried: {d}"
        );
        assert!(
            d.contains("c=6176"),
            "must name what the bisection proved: {d}"
        );
    }

    // The settle is proportional, free at the bottom, and capped — each is load-bearing: a flat
    // pause would waste time on small rungs, and an uncapped one has unpredictable duration.
    #[test]
    fn the_stream_settle_is_free_below_the_threshold_and_capped_above_it() {
        let ms = |c: u32| {
            if c <= STREAM_SETTLE_FREE_BELOW {
                0
            } else {
                (u64::from(c) * STREAM_SETTLE_MS_PER_1K / 1000).min(STREAM_SETTLE_MAX_MS)
            }
        };
        assert_eq!(ms(1), 0, "a single stream leaves nothing to drain");
        assert_eq!(
            ms(STREAM_SETTLE_FREE_BELOW),
            0,
            "at the threshold it is still free"
        );
        assert!(
            ms(STREAM_SETTLE_FREE_BELOW + 1) > 0,
            "and just above it, it is not"
        );
        assert!(
            ms(2048) > ms(1024),
            "it must scale with what the last window drove"
        );
        assert_eq!(ms(100_000), STREAM_SETTLE_MAX_MS, "and it must be bounded");
        // busbar's real case: the c=3,088 step-down that failed after c=6,144 passed clean.
        assert!(
            ms(6176) >= 6_000,
            "the rungs that actually broke must get a real pause, got {}",
            ms(6176)
        );
    }
}

#[cfg(test)]
mod stream_error_kind_tests {
    use super::*;
    use crate::http::{SseEnd, SseOutcome};

    fn outcome(status: Option<u16>, frames: usize, end: SseEnd) -> SseOutcome {
        SseOutcome {
            status,
            frames: vec!["x".into(); frames],
            frame_offsets_us: vec![1; frames],
            content_frames: frames as u64,
            end,
        }
    }

    // The ordering inside `add` is load-bearing: testing frames before status would file every
    // refused connection as "no frames", erasing the distinction this type exists to draw.
    #[test]
    fn each_error_is_filed_under_the_thing_that_actually_went_wrong() {
        let mut k = StreamErrorKinds::default();
        k.add(&outcome(
            None,
            0,
            SseEnd::ConnectionFailed("refused".into()),
        ));
        assert_eq!(
            (k.connect_failed, k.no_frames),
            (1, 0),
            "a refused connection must not be filed as 'answered 2xx but sent nothing'"
        );

        let mut k = StreamErrorKinds::default();
        k.add(&outcome(Some(503), 0, SseEnd::StreamClosed));
        assert_eq!(
            (k.status, k.no_frames),
            (1, 0),
            "a non-2xx is a status error, not a frame famine"
        );

        let mut k = StreamErrorKinds::default();
        k.add(&outcome(Some(200), 0, SseEnd::StreamClosed));
        assert_eq!(
            k.no_frames, 1,
            "a 2xx that delivered nothing is its own finding"
        );

        let mut k = StreamErrorKinds::default();
        k.add(&outcome(
            Some(200),
            3,
            SseEnd::NotAnEventStream("text/html".into()),
        ));
        assert_eq!(
            k.not_event_stream, 1,
            "a 2xx that is not SSE is a protocol error"
        );

        let mut k = StreamErrorKinds::default();
        k.add(&outcome(
            Some(200),
            3,
            SseEnd::Malformed("bad frame".into()),
        ));
        assert_eq!(
            k.not_event_stream, 1,
            "malformed shares the protocol-error class"
        );
    }

    /// The breakdown must account for every error, or a reader subtracting the parts from the total
    /// finds a remainder with no name and learns nothing.
    #[test]
    fn the_kinds_sum_to_the_errored_count() {
        let outcomes = [
            outcome(None, 0, SseEnd::ConnectionFailed("reset".into())),
            outcome(Some(502), 0, SseEnd::StreamClosed),
            outcome(Some(200), 0, SseEnd::Timeout),
            outcome(Some(200), 2, SseEnd::Malformed("x".into())),
        ];
        let mut k = StreamErrorKinds::default();
        let mut errored = 0u64;
        for o in &outcomes {
            if stream_errored(o) {
                errored += 1;
                k.add(o);
            }
        }
        assert_eq!(
            k.total(),
            errored,
            "every counted error must land in exactly one class"
        );
        assert_eq!(
            errored, 4,
            "this fixture must actually produce errors to be testing anything"
        );
    }

    /// Rig-side ends never reach the classifier - `stream_window` discards the whole window - so they
    /// must not be classifiable as a gateway error even if one leaked through.
    #[test]
    fn rig_side_ends_are_not_gateway_errors() {
        for end in [
            SseEnd::RigExhausted("EADDRNOTAVAIL".into()),
            SseEnd::RigRefused("no credential".into()),
        ] {
            assert!(
                !stream_errored(&outcome(None, 0, end)),
                "the rig running out is never the gateway's error rate"
            );
        }
    }
}

#[cfg(test)]
mod stepdown_tests {
    /// The step-down rule, extracted so it can be exercised without a rig. Mirrors the branch in
    /// `cell_streams`: bisect the bracket when there is one, halve only when there is not.
    fn next_rung(proven_clean: u32, lo: u32, ceiling: u32) -> u32 {
        let known_good = if proven_clean > 0 {
            proven_clean.max(lo)
        } else {
            lo
        };
        if known_good < ceiling {
            let mid = known_good + (ceiling - known_good) / 2;
            if mid >= ceiling {
                known_good
            } else {
                mid
            }
        } else {
            ceiling / 2
        }
    }

    // The defect this replaces: halving after a failed confirmation could step below a concurrency
    // already proven clean, discarding the whole bracket the search had paid for.
    #[test]
    fn the_step_down_stays_inside_the_bracket_the_search_paid_for() {
        let next = next_rung(4096, 1, 6176);
        assert!(
            next > 4096,
            "must not step below a rung this cell already carried cleanly, got {next}"
        );
        assert!(
            next < 6176,
            "must step below the rung that failed confirmation, got {next}"
        );
        assert_eq!(next, 5136, "bisects the bracket");
        assert_ne!(next, 3088, "the old halving answer must not survive");
    }

    /// Halving is still right when there is NO bracket - confirmation failed at the known-good rung
    /// itself, so nothing below it is known and there is nothing to bisect.
    #[test]
    fn halving_survives_only_where_there_is_no_information_to_bisect() {
        assert_eq!(
            next_rung(8192, 1, 8192),
            4096,
            "no bracket: fall back to halving"
        );
        assert_eq!(
            next_rung(0, 1, 1024),
            512,
            "nothing proven clean: bisect from the search floor"
        );
    }

    /// The search must always make progress downward, or it loops until the step-down budget runs
    /// out and publishes nothing - the exact failure this whole change exists to remove.
    #[test]
    fn every_step_makes_downward_progress() {
        for (pc, lo, ceil) in [
            (4096u32, 1u32, 6176u32),
            (100, 1, 101),
            (2, 1, 3),
            (1, 1, 2),
            (0, 1, 2),
        ] {
            let n = next_rung(pc, lo, ceil);
            assert!(
                n < ceil,
                "step-down from {ceil} (proven {pc}) must go DOWN, got {n}"
            );
        }
    }
}

#[cfg(test)]
mod gateway_recovery_tests {
    use super::*;

    // A gateway that stops serving after overload and doesn't come back is the gateway's own
    // finding — filing it under HarnessError would hide a real limitation behind our own fault.
    #[test]
    fn a_gateway_that_never_recovers_is_the_gateways_result() {
        assert_eq!(
            StreamStop::GatewayDidNotRecover {
                at: 4096,
                proven: 8192,
                restart_cleared: true
            }
            .absent_kind(),
            Absent::NotMeasured,
            "a gateway that will not serve again is not a rig fault"
        );
        // And the rig-side ones must stay rig-side, or this variant has laundered them.
        assert_eq!(
            StreamStop::RigContaminated {
                at: 3088,
                proven: 4096
            }
            .absent_kind(),
            Absent::HarnessError
        );
    }

    /// "Wedged until restarted" and "wedged and stayed wedged" are different claims about the
    /// gateway, and a reader deciding whether to run it in production needs to know which.
    #[test]
    fn the_reason_distinguishes_a_restart_that_helped_from_one_that_did_not() {
        let cleared = StreamStop::GatewayDidNotRecover {
            at: 4096,
            proven: 8192,
            restart_cleared: true,
        }
        .describe(8192, 6);
        let stuck = StreamStop::GatewayDidNotRecover {
            at: 4096,
            proven: 8192,
            restart_cleared: false,
        }
        .describe(8192, 6);
        assert!(
            cleared.contains("only after the harness restarted it"),
            "{cleared}"
        );
        assert!(stuck.contains("a restart did not bring it back"), "{stuck}");
        assert_ne!(cleared, stuck, "the two outcomes must not read identically");
        for d in [&cleared, &stuck] {
            assert!(d.contains("c=8192"), "must name what it had carried: {d}");
            assert!(d.contains("c=4096"), "must name the rung that failed: {d}");
        }
    }
}

#[cfg(test)]
mod restart_attribution_tests {
    use super::*;

    // The restart is an attribution test: its two outcomes blame opposite things. "Fixed it" means
    // the gateway was wedged; "did not fix it" means a fresh process still fails, leaving the host
    // as the only variable — the flattering direction, worth pinning with a test.
    fn stop_for(restart_cleared: bool, at: u32, proven: u32) -> StreamStop {
        if restart_cleared {
            StreamStop::GatewayDidNotRecover {
                at,
                proven,
                restart_cleared,
            }
        } else {
            StreamStop::RigContaminated { at, proven }
        }
    }

    #[test]
    fn a_restart_that_helps_blames_the_gateway_and_one_that_does_not_blames_us() {
        assert_eq!(
            stop_for(true, 4096, 8192).absent_kind(),
            Absent::NotMeasured,
            "a restart clearing it proves the old process was wedged - that is the gateway's"
        );
        assert_eq!(
            stop_for(false, 4096, 8192).absent_kind(),
            Absent::HarnessError,
            "a FRESH gateway still failing a rung it carried leaves the host as the variable - ours"
        );
    }

    /// Both reasons must name the rung and what the cell had carried, because the impossibility is
    /// the relation between them - either number alone reads like an ordinary failure.
    #[test]
    fn both_outcomes_explain_themselves_with_both_concurrencies() {
        for cleared in [true, false] {
            let d = stop_for(cleared, 4096, 8192).describe(8192, 6);
            assert!(d.contains("c=4096") && d.contains("c=8192"), "{d}");
        }
    }
}

// ── The search simulator ─────────────────────────────────────────────────────────────────────────
//
// Drives the real search (`sweep_streams_cell`) against synthetic gateways with a declared true
// ceiling, so assertions can check correctness (did it find the ceiling, and say so honestly when
// it couldn't) rather than plausibility against a real field run. The server models capacity by
// concurrently-open lanes, not accept order, so a window at c=N genuinely presents N simultaneous
// connections.
#[cfg(test)]
mod search_simulator {
    use super::*;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// How a synthetic gateway behaves as concurrent lanes pile up.
    #[derive(Clone, Copy, Debug)]
    enum Model {
        /// Serves cleanly up to `cap`, refuses everything above it. The easy case, and the one a
        /// bisection should nail exactly.
        KnifeEdge { cap: usize },
        /// Serves up to `cap`; past it, delivers a short stream instead of erroring — a delivery
        /// shortfall, which fails the gate without counting as an errored stream.
        Shortfall { cap: usize },
        /// Serves up to `cap`, but once it has ever seen more than `wedge_at` concurrent lanes it
        /// refuses everything from then on, permanently.
        Wedge { cap: usize, wedge_at: usize },
    }

    /// A synthetic gateway. Returns its address and the counter of peak concurrency it ever saw.
    fn sim_gateway(model: Model) -> SocketAddr {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = l.local_addr().expect("addr");
        let live = Arc::new(AtomicUsize::new(0));
        let wedged = Arc::new(AtomicUsize::new(0));
        std::thread::spawn(move || {
            for c in l.incoming() {
                let Ok(mut c) = c else { continue };
                let live = Arc::clone(&live);
                let wedged = Arc::clone(&wedged);
                std::thread::spawn(move || {
                    // Drop guard, not manual decrement per exit path: a hand-rolled version leaked
                    // and wedged the simulator itself, mimicking a search defect.
                    struct Lane(Arc<AtomicUsize>);
                    impl Drop for Lane {
                        fn drop(&mut self) {
                            self.0.fetch_sub(1, Ordering::SeqCst);
                        }
                    }
                    /* COUNT IN-FLIGHT REQUESTS, NOT OPEN SOCKETS. The engine drives a POOLED http
                    client, so idle connections stay open between windows and sit blocked in the
                    read below. Counting those as load meant the pool alone could exceed the
                    model's capacity, and every rung after the first failure was refused - the
                    simulator wedging itself and looking precisely like a search that had wedged.
                    Capacity is about requests being served, which is what this now measures. */
                    /* AND BOUND THE WRITES. When a window fails its gate the engine drops those
                    lanes, the socket buffer fills, and an unbounded `write_all` blocks in the
                    kernel FOREVER - holding its lane counted as in-flight for the rest of the
                    run. That is what made every rung after the first failure look refused: the
                    simulator's own stuck writers, not the model. */
                    let _ = c.set_write_timeout(Some(std::time::Duration::from_millis(250)));
                    let _ = c.set_read_timeout(Some(std::time::Duration::from_millis(250)));
                    let mut b = [0u8; 8192];
                    if c.read(&mut b).unwrap_or(0) == 0 {
                        return; // a pooled socket that never carried a request is not load
                    }
                    live.fetch_add(1, Ordering::SeqCst);
                    let _lane = Lane(Arc::clone(&live));
                    // Let the whole window arrive before judging it: deciding on arrival order
                    // instead of waiting a beat would measure scheduling, not concurrency.
                    std::thread::sleep(std::time::Duration::from_millis(60));
                    let n = live.load(Ordering::SeqCst);
                    let (cap, short, wedge_at) = match model {
                        Model::KnifeEdge { cap } => (cap, false, usize::MAX),
                        Model::Shortfall { cap } => (cap, true, usize::MAX),
                        Model::Wedge { cap, wedge_at } => (cap, false, wedge_at),
                    };
                    if n > wedge_at {
                        wedged.store(1, Ordering::SeqCst);
                    }
                    let stuck = wedged.load(Ordering::SeqCst) == 1;
                    let over = n > cap || stuck;
                    if over && !short {
                        // Refuse: a real non-2xx, which is what every observed failure actually was.
                        let _ = c.write_all(b"HTTP/1.1 503 Busy\r\ncontent-length: 0\r\n\r\n");
                        return;
                    }
                    let _ =
                        c.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n");
                    let _ = c.write_all(
                        b"data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n",
                    );
                    let frames = if over {
                        1
                    } else {
                        crate::metric::STREAM_FRAME_BUDGET
                    };
                    for i in 0..frames {
                        let _ = c.write_all(
                            format!(
                                "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"f{i}\"}}}}]}}\n\n"
                            )
                            .as_bytes(),
                        );
                    }
                    // Hold the lane until the client stops reading, so a window's lanes genuinely
                    // overlap. Bounded by the read timeout, so an abandoned lane cannot pile up.
                    let mut sink = [0u8; 64];
                    let _ = c.read(&mut sink);
                });
            }
        });
        addr
    }

    fn search(model: Model, lo: u32, hi: u32) -> CellStreams {
        let gw = sim_gateway(model);
        // The mock needs its own server: sharing one address with the gateway would let the
        // direct-to-mock reference's lanes count as gateway load in the capacity model.
        let mock = sim_gateway(Model::KnifeEdge { cap: usize::MAX });
        let cfg = super::tests::cfg_for(gw, mock);
        let id = CellId::new("openai", "openai");
        sweep_streams_cell(&cfg, &id, lo, hi)
    }
    // ── the assertions ──────────────────────────────────────────────────────────────────────────
    // Each checks correctness against a declared ceiling rather than plausibility of the output.

    // The model's ceiling is approximate (lane ramp means a window may not present its full
    // concurrency simultaneously), so the bar is "within a quarter" — loose enough to survive that
    // slack but tight enough that a 2x search error can't hide inside it.
    const SLACK: f64 = 1.25;

    /// The base case. A gateway with a hard ceiling must be measured at about that ceiling.
    #[test]
    fn a_knife_edge_ceiling_is_found() {
        for cap in [24usize, 40] {
            let r = search(Model::KnifeEdge { cap }, 1, 128);
            let got = r
                .concurrency
                .value()
                .copied()
                .expect("a hard ceiling must yield a number");
            let lo = (cap as f64 / SLACK) as u32;
            let hi = (cap as f64 * SLACK) as u32;
            assert!(
                (lo..=hi).contains(&got),
                "declared ceiling {cap}, search published {got} - outside [{lo}, {hi}]"
            );
        }
    }

    // The halving defect: a search that discards its bracket on step-down can land far under the
    // true ceiling. Answer must be at/above what was already proven, not below.
    #[test]
    fn the_search_never_lands_below_a_concurrency_it_already_carried() {
        for cap in [12usize, 20, 33] {
            let r = search(Model::KnifeEdge { cap }, 1, 128);
            let got = r.concurrency.value().copied();
            if let Some(v) = got {
                assert!(
                    (v as f64) >= cap as f64 / SLACK,
                    "declared {cap}, published {v} - a step-down that discards its bracket lands here"
                );
            }
            // Every rung it probed and passed is a rung it carried; none may exceed the true cap.
            for p in &r.points {
                if p.passed {
                    assert!(
                        (p.concurrency as f64) <= cap as f64 * SLACK,
                        "passed at c={} on a gateway capped at {cap} - beyond the harness's ramp slack",
                        p.concurrency
                    );
                }
            }
        }
    }

    // The premature-termination defect: a gateway with a real, findable ceiling must not come back
    // empty just because one stepped rung failed a single window.
    #[test]
    fn a_findable_ceiling_is_never_published_as_nothing() {
        for cap in [9usize, 17, 40] {
            let r = search(Model::KnifeEdge { cap }, 1, 128);
            assert!(
                r.concurrency.value().is_some(),
                "gateway with a hard ceiling at {cap} published NO number - the search gave up while \
                 an answer was available (this is plano anthropic>anthropic)"
            );
        }
    }

    /// A delivery shortfall fails the gate with zero errors; a search that only understands errored
    /// streams would climb straight past it.
    #[test]
    fn a_delivery_shortfall_bounds_the_ceiling_just_like_an_error_does() {
        let r = search(Model::Shortfall { cap: 16 }, 1, 64);
        for p in &r.points {
            if p.passed {
                assert!(
                    (p.concurrency as f64) <= 16.0 * SLACK,
                    "c={} passed against a gateway that goes short above 16",
                    p.concurrency
                );
            }
        }
    }

    // The wedge: once the gateway stops serving, the only correct outcomes are no number, or a
    // number at or below the true cap. A number above the cap would publish the wedge as capacity.
    #[test]
    fn a_wedged_gateway_never_yields_a_number_above_its_real_ceiling() {
        let r = search(
            Model::Wedge {
                cap: 16,
                wedge_at: 32,
            },
            1,
            128,
        );
        if let Some(v) = r.concurrency.value().copied() {
            assert!(
                (v as f64) <= 16.0 * SLACK,
                "gateway wedges above 32 and truly carries 16, but the search published {v}"
            );
        }
    }

    /// Whatever the model, a published rung must have actually held its own windows in the trace.
    /// The summary and the evidence cannot be allowed to disagree.
    #[test]
    fn a_published_rung_held_a_majority_of_its_own_windows_in_every_model() {
        for m in [
            Model::KnifeEdge { cap: 20 },
            Model::Shortfall { cap: 20 },
            Model::Wedge {
                cap: 12,
                wedge_at: 48,
            },
        ] {
            let r = search(m, 1, 64);
            let Some(v) = r.concurrency.value().copied() else {
                continue;
            };
            let at: Vec<bool> = r
                .points
                .iter()
                .filter(|p| p.concurrency == v)
                .map(|p| p.passed)
                .collect();
            assert!(
                !at.is_empty(),
                "{m:?}: published c={v} with no window at that concurrency"
            );
            assert!(
                at.iter().filter(|x| **x).count() * 2 > at.len(),
                "{m:?}: published c={v} on {:?} - not a majority",
                at
            );
        }
    }
}
