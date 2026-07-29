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
    /// The gateway's declared capability grid and the cells the rig cannot pose. See
    /// `Manifest::matrix` / `Manifest::untestable`. Empty `matrix` means undeclared: every cell is
    /// probed, unchanged from before this field existed.
    pub matrix: Vec<String>,
    pub matrix_note: String,
    pub untestable_cells: Vec<String>,
    pub untestable_note: String,
    /// HOW TO PUT THE GATEWAY BACK AT REST. The memory group needs a process that has not served
    /// load to read an idle RSS from, and the only way to get one is to restart it, so the spec that
    /// launched it has to be reachable from a metric.
    ///
    /// `None` when the harness does not own the gateway's lifetime (no `launch` in the manifest, or
    /// a run against an already-up target). The memory group then publishes idle as ABSENT rather
    /// than as a reading it knows was taken under load - see `Memory::measure`.
    pub relaunch: Option<crate::launch::LaunchSpec>,
    /// The manifest's post-boot `commands`, REPLAYED ON EVERY RESTART. A gateway with no config
    /// file is configured through its own admin API after it boots, and for docker a stop is
    /// `docker rm -f`: the container's writable layer - the database those commands wrote - is
    /// destroyed with it. Restarting without replaying them relaunches an UNCONFIGURED gateway:
    /// on the 2026-07-28 board one-api lost its three channels at the memory group's restart, and
    /// every metric measured after it (streaming, added latency) failed 100% while throughput,
    /// measured before it, published real numbers - a half-configured gateway answering probes is
    /// exactly the state the initial-launch path refuses to measure.
    pub relaunch_commands: Vec<String>,
    /// THE ONE LAUNCHER THAT OWNS THIS GATEWAY'S NATIVE CHILD, for every restart across every cell.
    ///
    /// `restart_to_rest` used to build a throwaway `RealLauncher` per call: the `Child` it held was
    /// dropped when the function returned, so the NEXT restart's `pkill` killed a process nothing
    /// could `wait()` on, leaking a zombie process-table entry once per served cell over an
    /// eight-hour run. Holding the same launcher here, across every cell, means the launcher that
    /// spawned a native child is still the one asked to stop it next time, so it can actually reap
    /// it (see `RealLauncher::reap_previous_native_child`). Present even when `relaunch` is `None`;
    /// it is simply never used in that case.
    pub relaunch_launcher: std::sync::Mutex<crate::launch::RealLauncher>,
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
/// The model name this cell must send to reach ITS egress column.
///
/// Every request the grid makes goes through here rather than reading `cfg.model`, because reading
/// the bare field is exactly the defect this exists to prevent: most gateways pick the upstream from
/// the model name, so a fixed model sends the same request for all six egress columns, reaches one
/// upstream, and publishes six cells for one measurement - a translation claim the gateway was never
/// asked to perform. Falls back to the declared `model` when the manifest names nothing for this
/// column, which is right for a single-upstream gateway and for the column whose canonical name is
/// already the declared one.
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
/// ONE HEADER PER NAME ON THE WIRE, and the rig's own copy is the one that goes.
///
/// Ledger RIG-12's remainder. This used to concatenate all three sources unfiltered, and nothing
/// stopped a manifest declaring a name the dialect already sends - `gateways/litellm-rust` declares
/// `Authorization: Bearer {GW_AUTH}`, which collides with the bearer header the openai,
/// openai-responses, cohere and bedrock ingress dialects each send. Two `authorization` headers left
/// on one request, and HTTP does not define which a server honours: first, last, or comma-joined and
/// rejected, by implementation. Nothing errors, the gateway authenticates as SOMEBODY, and a clean
/// number is published for a request whose credential and therefore whose tenant and route we cannot
/// state. A wrong measurement that looks entirely right.
///
/// The dialect's wins because the credential is the HARNESS's to assert: `cfg.auth` is the token
/// this run holds (one gateway mints it at launch), and the shape is what a real client of that
/// dialect sends. A manifest able to override it could have a gateway measured under an identity the
/// harness cannot name. Disclosed rather than silent: `Manifest::rig_owned_headers_declared` reports
/// the collision through `otb validate`, naming the file and the header.
///
/// Dropping, not refusing at load, because the one manifest that trips this is a first-party file
/// and refusing would stop the whole benchmark rather than measure it unambiguously. The comparison
/// is exact today - that manifest's value is byte-identical to the header it duplicates - so this
/// changes what goes on the wire only in the case that was undefined anyway.
pub(crate) fn headers_for(
    cfg: &RunConfig,
    ingress: Dialect,
    egress: &str,
) -> Vec<(String, String)> {
    let mut out = ingress.auth_headers(&cfg.auth);
    // Case-insensitively, because HTTP header names are (`HttpResponse::header` already reasons this
    // way) and `Authorization` vs `authorization` is the same header to every server that reads it.
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

/// Ask the gateway whether it serves this pairing. The answer comes only from what was OBSERVED:
/// THE MOST SIMULTANEOUS CONNECTIONS THIS HOST CAN ACTUALLY MAKE TO ONE DESTINATION.
///
/// A TCP connection is identified by (src ip, src port, dst ip, dst port). Every load window drives
/// ONE destination, so simultaneous connections are bounded by this host's ephemeral source ports:
/// `net.ipv4.ip_local_port_range`. Asking past it does not measure a bigger gateway - `connect`
/// starts returning EADDRNOTAVAIL, and before `GenStats::rig_refused` existed those landed in the
/// failure count where nothing could tell them apart from the gateway refusing, so the search would
/// publish the rig's port range as the gateway's ceiling.
///
/// EVERY NUMBER HERE IS READ OR DERIVED, none chosen. The ceiling is the largest power of two that
/// fits the host's own range - powers of two because the ladder doubles, so a ceiling between rungs
/// would be reachable only by the clamp and would make the top rung a different shape from every
/// rung below it.
///
/// The fallback, when /proc cannot be read (macOS, a restricted container), is the same computation
/// over Linux's own documented default range rather than a constant somebody picked: if we cannot
/// ask the host, we assume the host is stock.
///
/// TIME_WAIT is not handled by shaving a fraction off - that would be an invented number doing a
/// real job badly. A closed connection holds its port until TIME_WAIT expires, so the orchestrator
/// enables `net.ipv4.tcp_tw_reuse` before a run and the kernel recycles them for new outbound
/// connections. Widening the range or changing that policy needs no change here: this reads whatever
/// the host is actually configured to do.
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

/// The concurrency ceiling for a HELD-OPEN STREAM, which is a different physical bound from a request.
///
/// `host_connection_ceiling` derives its answer from the ephemeral port range, and for short requests
/// that is exactly right: a request opens a socket, completes in milliseconds, and hands the port
/// back, so how many can be in flight at once is a question about ports.
///
/// A streaming lane does not do that. It holds its connection open for the whole window - the mock
/// paces 64 frames at 20ms, so about 1.28 seconds - and for that entire time it also holds a task, a
/// read buffer, and a file descriptor on the rig, one on the gateway, and one on the mock. The
/// binding resource is therefore DESCRIPTORS, not ports, and descriptors are the smaller number by a
/// wide margin on a stock box.
///
/// The 2026-07-29 run inherited the port bound and climbed apisix's stream ladder to c=32768 on both
/// of its streamable cells. Forty-nine thousand usable ports made that the largest power of two the
/// port rule allowed, so the ladder ran to the top, never plateaued - because past a few thousand
/// held-open streams the RIG is what is saturating, and a saturating rig keeps yielding small
/// increments rather than the flat run the search stops on - and published nothing at all. Thirty-two
/// thousand concurrently-held SSE streams on one box is not a measurement of a gateway.
///
/// A THIRD OF THE DESCRIPTOR BUDGET, not all of it: the rig needs descriptors for everything else it
/// is doing, and a ladder that climbs until the process runs out of file handles measures the ladder.
/// Still capped by the port rule, which remains a real bound and is simply not the first one to bite.
///
/// AND A RUNAWAY BACKSTOP, WHICH IS NOT THE SAME AS A MEASUREMENT BOUND.
///
/// An earlier version of this put a constant ceiling at 4096, reasoning that no gateway had cleanly
/// exceeded c=2178. The field's own sweeps said otherwise: apisix sustained c=16384 with ZERO stalls
/// and every window passing, litellm-rust c=6144, aisix c=4096. That constant would have clipped
/// three gateways and published a smaller rung as their peak - a wrong number, which is worse than an
/// honest hole. A ceiling chosen near where measurements live becomes part of the measurement.
///
/// So the ladder's real stopping condition is MEASURED: it climbs until its rungs stop holding, and
/// `saturation_plateau` publishes the best rung that actually passed (see the bound-versus-ceiling
/// note there). `STREAM_RUNAWAY_CAP` exists only so a bug cannot climb forever - it sits far above
/// anything this field has produced or plausibly could, so it never participates in a result. If a
/// search ever reaches it, that is a runaway to investigate and not a gateway's ceiling to publish.
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

/// The same probe over an EXPLICIT budget, so a test can exercise the retry loop without sleeping
/// through the field's real pause.
///
/// The pause is a minute of wall clock across the budget. Tests that sat through it did not just run
/// slowly, they held sockets while they waited and starved the rest of the suite, which is how a
/// neighbouring stream test started failing under parallel load - a test made flaky by another test
/// is worse than a slow one, because the red it produces points at innocent code. `supervise.rs`
/// already injects its own sleep for exactly this reason; this follows it.
pub fn probe_cell_within(
    cfg: &RunConfig,
    id: &CellId,
    mock_healthy: bool,
    attempts: u32,
    pause: Duration,
) -> Served {
    let (mut last, mut retryable) = probe_cell_once(cfg, id, mock_healthy);
    // SPEND THE BUDGET THE VERDICT CLAIMS TO HAVE SPENT.
    //
    // `Verdict::Failed` is documented as "the failure persisted across the whole budget", and
    // `transient_budget()` exists to fund that, but nothing outside its own tests ever called it: a
    // single 503 was recorded as "this gateway does not serve this pairing", permanently, on the
    // board. A status that says TEMPORARILY unavailable in words is not a capability.
    //
    // The harness makes this condition itself. Cells run back to back with no settle and the metric
    // before each probe is a heavy load, so a gateway with admission control can still be shedding
    // when the next cell asks whether it exists. busbar answered 503 on 26 of 36 cells in the
    // 2026-07-28 field run and every one was published as a red; the day before, on a lighter
    // engine, it served all 36. Its egress lanes all still answered under openai ingress in the
    // same run, which is what shows the lanes were healthy and the moment was not.
    //
    // Every cell gets the same attempts and the same pause - the budget takes no arguments for that
    // reason - so no cell can be tried harder than another.
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

/// One probe attempt: the verdict, and WHY it is worth asking again, if it is.
///
/// The second half is what `probe_cell` spends the budget on. Two kinds of answer deserve another
/// ask, and they arrive through different doors:
///
///   - a transient STATUS (503 and friends), which is the gateway saying "not right now"
///   - a transient TRANSPORT failure (no answer at all, or a refused connection), which is the
///     gateway not saying anything right now
///
/// Both are moments. The harness manufactures both: cells run back to back with no settle and the
/// metric before each probe is a heavy load, so the next cell asks its question of a gateway still
/// shedding. busbar lost 26 cells to the first door in the 2026-07-28 field run and litellm-python
/// lost three to the second, its served count sliding 8 -> 7 -> 5 across the day's runs while the
/// cells it lost were recorded "the gateway accepted the connection and never answered".
///
/// A malformed response and an unknown dialect are NOT retryable: the first is a real answer the
/// gateway keeps giving, the second is our own manifest and no amount of asking changes it.
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
            // WHAT IT ACTUALLY SAID. Without this a declined cell is a bare verdict, and a whole
            // field answering 4xx for one rig-side reason reads as every gateway supporting nothing.
            let evidence = crate::cell::Evidence {
                status: r.status,
                body_snippet: crate::cell::Evidence::snippet(&String::from_utf8_lossy(r.body())),
            };
            // A REFUSAL WE PROVOKED IS NOT A CAPABILITY VERDICT. A real client of some dialects signs
            // its requests and the harness cannot: it sends a bearer token and will not forge a
            // signature. A gateway that checks credentials properly answers 401/403 to that, which is
            // CORRECT behaviour, so grading it as a refusal would publish a red the gateway did not
            // earn. Decided here rather than in `persistent_transient_verdict` because that function
            // is a pure function of the observed status and must stay one - this needs the dialect,
            // which is a property of our own instrument, not of the gateway.
            if ing.auth_is_unforgeable_by_the_rig() && matches!(r.status, 401 | 403) {
                return (Served::UnprobedAuth(evidence), None);
            }
            // The verdict decides which of the three this is, and they are NOT interchangeable.
            // NotConfigured is the gateway's own answer that the pairing does not exist. Failed is
            // the gateway's own answer that it reached and declined this attempt at a pairing that
            // is otherwise real. NotVerified means the rig could not get a fair reading, so nothing
            // was learned about the gateway, and recording it as "does not serve" would convict on
            // the rig's failure.
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
        // No HTTP answer at all: the gateway may never have been reached, so this says nothing
        // about it. Never a gateway fault.
        Outcome::ConnectionFailed(e) => (
            Served::Untestable(format!("no connection to the gateway: {e}")),
            Some("the connection was refused".to_string()),
        ),
        Outcome::TimedOut => (
            Served::Untestable("the gateway accepted the connection and never answered".into()),
            Some("the gateway did not answer in time".to_string()),
        ),
        // A response we cannot parse is a real answer the gateway keeps giving; asking again spends
        // the budget to be handed the same bytes.
        Outcome::Malformed { message, .. } => (
            Served::Untestable(format!("unparseable response: {message}")),
            None,
        ),
        // WE NEVER ASKED, so there is nothing here to grade. `Untestable` and never `Served::No`:
        // every other arm above describes something the GATEWAY did, and this one describes a
        // manifest of ours that declared a header we will not put on the wire. Recording it as a
        // capability verdict would convict a gateway of our defect, and `probe::Verdict` has the
        // right word for it already - NotVerified is "a statement about the RIG, not the gateway",
        // which `Untestable` is how this function spells.
        //
        // No retry: the manifest will say the same thing on the next attempt, so the budget would be
        // spent to be refused identically. It is loud on stderr so the run points at the file to fix.
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
    /// The SAME composed header list the probe authenticated this cell with. Carried per probe rather
    /// than rebuilt inside `spawn_pinned`, so the window and the probe cannot end up speaking to the
    /// gateway as two different clients.
    headers: Vec<(String, String)>,
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
        // THE RIG RUNNING OUT IS NOT A GATEWAY RESULT, and it is the same class of fact
        // `spawn_failed` already models: the window never ran at the concurrency it claims, so
        // nothing about the gateway was learned at any concurrency we could name.
        //
        // These are connections THIS HOST could not make - ephemeral ports or descriptors exhausted
        // (EADDRNOTAVAIL/EMFILE). They used to land in `fail` beside a genuine refusal, so the gate
        // failed and the search recorded our own port range as the gateway's ceiling. Counting them
        // separately was only half the fix: a window containing any of them still failed, and still
        // failed for our reason. It is unmeasured instead.
        if stats.rig_refused > 0 {
            eprintln!(
                "loadgen: could not reach c={concurrency}; this host refused {} of its own connections \
                 (ephemeral ports or descriptors exhausted) - the window never ran at that concurrency",
                stats.rig_refused
            );
            return None;
        }
        // A window that produced nothing is UNMEASURED, not a zero.
        if stats.ok == 0 && stats.fail == 0 {
            return None;
        }
        // THE LATENCY RIDES ALONG WITH THE RATE, because the generator already measured it.
        //
        // This used to return the rate and the verdict alone, and the p99 this window ran at died
        // here. That single narrowing is why the engine ran a SECOND search to answer "and how much
        // at 20ms?": the answer was not readable off the sweep it had just taken. The second search
        // ran after the memory group had restarted the gateway, so the two published throughput
        // numbers described two different states of it. Carrying the reading is what lets one sweep
        // answer both, from one set of windows, on one state of the gateway.
        Some(
            Sample::new(stats.rps() as f64, stats.fail == 0 && stats.ok > 0).with_reading(
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
    // The stop above already confirmed the previous native child (if any) is dead, so reaping it
    // here through the SAME launcher that spawned it is a wait, never a hang - and it is the only
    // safe way this process can collect a pid the next `pkill` is about to kill.
    launcher.reap_previous_native_child();
    crate::launch::launch_default(&mut *launcher, spec)
        .map(|_| ())
        .map_err(|e| format!("it did not come back up: {e:?}"))?;
    // REPLAY THE POST-BOOT COMMANDS, exactly as the initial launch ran them. For docker the stop
    // above was `docker rm -f`: any configuration those commands wrote into the container (an
    // admin-API-configured gateway's database - its channels, its quota) died with the writable
    // layer, and a gateway that comes back up unconfigured answers probes while serving nothing.
    // A failure here is the restart failing, not a softer state: a half-configured gateway is
    // worse than a down one, because every later metric would measure the missing configuration
    // and publish it as the gateway's own failure.
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

/// The same load window, driven at an EXPLICIT address rather than the gateway's.
///
/// The added-latency group's baseline leg has to put load on the mock directly, using the exact same
/// generator, pinning and windowing as every gateway-facing window - otherwise the two legs of a
/// difference would be two different measuring instruments and the gap between them would be partly
/// rig noise rather than purely what the gateway adds. `load_window` stays the common case (there is
/// no second address to thread through every existing call site), and is now a one-line call into
/// this.
///
/// `headers` IS NOT OPTIONAL AND IS NOT DERIVED HERE. The child used to hardcode
/// `authorization: Bearer dummy`, so every load window authenticated as a placeholder while the probe
/// beside it used the manifest's real credential in the right per-dialect shape. A gateway whose
/// declared auth was anything else passed its probe and then failed 100% of every window, and the
/// absence that reached the artifact blamed the search rather than naming a credential fault. Taking
/// the composed header list as an argument, from the same `headers_for` the probe uses, is what makes
/// that impossible to reintroduce by forgetting a field.
pub fn load_window_at(
    cfg: &RunConfig,
    addr: SocketAddr,
    path: &str,
    body: &str,
    headers: &[(String, String)],
    concurrency: u32,
) -> Option<GenStats> {
    {
        let exe = std::env::current_exe().ok()?;
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
            // The credential rides in the ENVIRONMENT, not the argument list: a token on a command
            // line is visible in `ps` to every user on the box for the life of the window.
            .env(
                crate::loadgen::HEADERS_ENV,
                crate::loadgen::encode_headers(headers),
            )
            .stderr(std::process::Stdio::inherit())
            .output()
            .ok()?;
        let line = String::from_utf8_lossy(&out.stdout);
        let parsed = crate::loadgen::parse_ugen_line(line.trim());
        // OUR OWN WIRE CONTRACT BREAKING IS NOT AN EMPTY WINDOW.
        //
        // `parse_ugen_line` classifies a stats line missing a required field, or carrying a
        // non-numeric one, as `HarnessError` with a detail naming the field and quoting the line -
        // and `.into_value()` erased both one call later. `None` then travels up through
        // `load_window`, `SweepProbe::probe` and the search until the cell publishes
        // `NotMeasured("no load window completed at c=X")`, the same message an idle or killed window
        // gets. The one piece of evidence that would tell an operator "this is the engine
        // disagreeing with its own loadgen child, not the gateway and not the rig" was generated and
        // then thrown away.
        //
        // Reported rather than threaded: the value still has to become `None` here, because there is
        // no window to report on, but the reason it is `None` now reaches stderr and the run log
        // instead of dying at this line.
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
            elapsed_s: if u.rps > 0 {
                u.ok as f64 / u.rps as f64
            } else {
                0.0
            },
            latencies_us: Vec::new(),
            spawn_failed: false,
            rig_refused: u.rig_refused.max(0) as u64,
            budget_exceeded: u.budget_exceeded.max(0) as u64,
            // The subprocess never sends its raw samples back, only the percentiles it already
            // computed over them, so these are filled straight from the stats line rather than left
            // for a caller to (wrongly) derive from the now-empty `latencies_us` above.
            p50_us: Some(u.p50_us.max(0) as u64),
            p99_us: Some(u.p99_us.max(0) as u64),
        })
    }
}

pub struct CellPerf {
    pub max_proxy: Measurement<f64>,
    pub max_proxy_concurrency: Measurement<u32>,
    /// THE SAME SWEEP'S ANSWER TO THE SECOND QUESTION: how much throughput the cell held while its
    /// p99 stayed under `SUSTAINED_P99_CEILING_US`.
    ///
    /// Read off the rungs `max_proxy` was chosen from, not measured by a search of its own. That is
    /// the whole point: a reader comparing these two numbers is comparing two summaries of ONE set
    /// of windows taken on ONE state of the gateway, so "the sustained figure exceeds the maximum"
    /// is not a threshold to police, it is arithmetic that cannot happen. When the two were separate
    /// searches the second ran after the memory group had restarted the gateway, and on three cells
    /// of the 2026-07-28 run it came out above the first.
    pub sustained: Measurement<f64>,
    pub sustained_concurrency: Measurement<u32>,
    /// The rungs as the GATE saw them, plus every window spent refining the boundary. Published as
    /// `sweep_sustained_20ms`, so the sustained figure carries its own evidence exactly as the peak
    /// carries the sweep it came out of.
    pub sustained_points: Vec<SustainedPoint>,
    /// EVERY concurrency the search actually probed, in probe order. The peak is one point out of
    /// this; without it the published number cannot be re-derived or charted, and a reader has no
    /// way to see whether the search found a real turnover or simply ran out of range.
    pub points: Vec<crate::search::ProbedPoint>,
}

/// The 20ms gate as a predicate over one window, for the climb to judge every rung against.
///
/// A thin wrapper over `sustained_gate_passes` rather than a second copy of the rule: the boundary
/// this whole metric turns on stays in exactly one place, and the free function stays unit-testable
/// against fixed numbers.
fn sustained_gate() -> impl Fn(&crate::search::Reading) -> bool {
    |r: &crate::search::Reading| sustained_gate_passes(r.p99_us, r.ok, r.fail)
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
    let mut p = SweepProbe {
        cfg,
        path: path_for(cfg, ing, &id.egress),
        body: ing.body(&model_for(cfg, &id.egress)),
        headers: headers_for(cfg, ing, &id.egress),
    };
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

/// Find the gateway's throughput peak on one served cell, AND how much of it survives the 20ms gate.
///
/// ONE SWEEP, TWO ANSWERS. Users ask two questions about a gateway - "how much can it do?" and "how
/// much can it really do, at a latency I would accept?" - and those only mean anything side by side
/// if they describe the same gateway at the same moment. They used to be two searches: this one, and
/// a `bisect_ceiling` that ran three groups later, after `Memory` had cold-restarted the gateway and
/// driven minutes of load through it. Each number was a real measurement; the PAIR was a comparison
/// between two different states of the process, and on three cells of the 2026-07-28 run the
/// "sustained" figure landed above the "maximum" one by up to 7%.
pub fn sweep_cell(cfg: &RunConfig, id: &CellId, lo: u32, hi: u32) -> CellPerf {
    let Ok(ing) = id.ingress.parse::<Dialect>() else {
        return CellPerf {
            max_proxy: Measurement::absent(Absent::Untestable),
            max_proxy_concurrency: Measurement::absent(Absent::Untestable),
            sustained: Measurement::absent(Absent::Untestable),
            sustained_concurrency: Measurement::absent(Absent::Untestable),
            sustained_points: Vec::new(),
            // Nothing was probed, so there is no evidence to carry.
            points: Vec::new(),
        };
    };
    let mut p = SweepProbe {
        cfg,
        path: path_for(cfg, ing, &id.egress),
        body: ing.body(&model_for(cfg, &id.egress)),
        headers: headers_for(cfg, ing, &id.egress),
    };
    // No start argument: `saturation_plateau` always climbs from the floor. A start derived from the
    // range made the ladder arbitrary and made a WIDER range open with a HIGHER first probe, which is
    // how a 1..65536 run began by asking for 32768 concurrent connections.
    let gate = sustained_gate();
    let r = search::saturation_plateau_gated(&mut p, lo, hi, Some(&gate));
    let refined = refine_ceiling(&mut p, &r.rungs);
    match r.peak.value() {
        Some(pt) => CellPerf {
            max_proxy: Measurement::Measured(pt.value),
            max_proxy_concurrency: Measurement::Measured(pt.concurrency),
            sustained: refined.rps,
            sustained_concurrency: refined.concurrency,
            sustained_points: refined.points.clone(),
            points: r.points.clone(),
        },
        None => CellPerf {
            // A climb that established no peak established no sustained figure either: the sustained
            // number is a SUMMARY OF THE SAME RUNGS, so it cannot outlive them. Publishing one here
            // would be the exact defect this function was rewritten to remove, in miniature.
            sustained: Measurement::absent(r.peak.reason().cloned().unwrap_or(Absent::NotMeasured)),
            sustained_concurrency: Measurement::absent(
                r.peak.reason().cloned().unwrap_or(Absent::NotMeasured),
            ),
            // The rungs still explain WHY nothing was established, so they travel even here.
            sustained_points: refined.points.clone(),
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

/// The exact concurrency where the 20ms gate breaks, refined INSIDE the bracket the climb proved.
///
/// The ladder doubles, so the climb only ever proves "held at 64, failed at 128". Publishing 64 as
/// the ceiling would round every gateway down to a power of two, and the 2026-07-28 run shows how
/// much that costs: bifrost openai>openai sustained c=13, which the ladder would have called 8.
/// So the bracket is bisected - but with windows taken HERE, seconds after the rungs that defined
/// it, against the same process. That is the difference from the search this replaces, which found
/// the same boundary exactly but did it three groups and one gateway restart later.
///
/// The winner is then confirmed over `WINDOWS_PER_RUNG`, because a bisection lands by construction
/// on the highest concurrency that passed ONCE, and a ceiling a gateway holds a third of the time is
/// not a ceiling it sustains. A confirmation that fails steps down and confirms again, bounded,
/// since each step at least halves the distance back to a concurrency already known to hold.
///
/// A bracket only exists when the climb SAW a rung fail. When the gate was still holding at the top
/// of the range there is no boundary to refine and no ceiling to publish - only a lower bound, which
/// this returns as `SearchExhausted` rather than dressing our own `max_conc` up as the gateway's
/// answer. `bisect_ceiling` has always refused that case; this is the same refusal.
fn refine_ceiling<P: crate::search::Probe>(
    p: &mut P,
    rungs: &[crate::search::RungSummary],
) -> Refined {
    let mut points: Vec<SustainedPoint> = Vec::new();
    // EVERY RUNG THE CLIMB JUDGED IS EVIDENCE FOR THIS NUMBER, not just the windows spent refining
    // the boundary. Publishing only the refinement would show a reader the last two probes and hide
    // the curve that decided where to probe.
    for r in rungs {
        // WHAT THE RUNG MEASURED, WITH THE VERDICT IT EARNED - not a correct verdict beside invented
        // companion numbers. Every climb rung used to publish `p99_us: None, fail: 0` and a rate read
        // off `gate_median`, which is the median of the windows that HELD the gate - so a rung that
        // failed, having held in none of its windows, published `rps: 0` from that field's
        // `unwrap_or(0.0)`. A reader re-deriving the verdict from `sweep_sustained_20ms` saw a rung
        // that served nothing and lost nothing, when it had in fact served (say) 15000 rps at a 30ms
        // p99 and been failed for exactly that. The failing rung's evidence is the evidence for the
        // ceiling sitting below it, and it was the one row with no real numbers on it.
        //
        // A holding rung still publishes its GATE median - the rate it sustained under the ceiling -
        // and a failing rung publishes its own median, the rate it served at while blowing it. Each
        // comes with the reading of the window that produced it, so the row is one window's evidence
        // rather than three windows' numbers assembled into a measurement none of them made.
        let (rps, window) = if r.gate_holds {
            (
                r.gate_median.unwrap_or(r.median),
                r.gate_median_reading.or(r.median_reading),
            )
        } else if r.windows == 0 {
            // NO WINDOW AT THIS RUNG PASSED, SO ITS `median` IS A SENTINEL AND NOT A RATE.
            // `search::measure_rung` has no median to take when every window failed, so it returns
            // 0.0 - and this row used to publish that 0 beside a null p99 and a null loss count, a
            // rung that read as having served nothing when the windows had watched it serve at full
            // rate and drop requests. What it was OBSERVED doing is the evidence, and the reading of
            // the window behind it travels so the row stays one window's account of itself.
            (
                r.observed_median.unwrap_or(r.median),
                r.observed_median_reading.or(r.median_reading),
            )
        } else {
            (r.median, r.median_reading)
        };
        points.push(SustainedPoint {
            concurrency: r.concurrency,
            passed: r.gate_holds,
            rps,
            p99_us: window.and_then(|w| w.p99_us),
            // A rung whose windows all came back without a reading has no loss count to publish, and
            // now says so: `None` travels to `metric.rs` as an absence rather than as the zero this
            // used to invent. Every rung the load generator actually measures carries a reading, so
            // this is the rare path - which is exactly why a fabricated 0 here would have gone
            // unnoticed while reading as a clean rung.
            fail: window.map(|w| w.fail as i64),
        });
    }
    let held: Vec<&crate::search::RungSummary> = rungs.iter().filter(|r| r.gate_holds).collect();
    let Some(top) = held.last() else {
        // NOTHING HELD THE GATE AT ANY CONCURRENCY, INCLUDING THE FLOOR. That is a measured answer
        // and not an absence: the gateway was asked and never once kept its p99 under the ceiling.
        return if rungs.is_empty() {
            Refined {
                rps: Measurement::absent(Absent::NotMeasured),
                concurrency: Measurement::absent(Absent::NotMeasured),
                points,
            }
        } else {
            Refined {
                rps: Measurement::Measured(0.0),
                concurrency: Measurement::Measured(0),
                points,
            }
        };
    };
    let lo = top.concurrency;
    // The first rung ABOVE the last holder that actually failed. Absent when the climb ran out of
    // range while still holding: then the ceiling is a lower bound, and `lo` is the honest answer we
    // can defend rather than a boundary we never saw.
    let hi = rungs
        .iter()
        .find(|r| r.concurrency > lo && !r.gate_holds)
        .map(|r| r.concurrency);
    let Some(hi) = hi else {
        // THE GATE STILL HELD AT THE TOP OF THE RANGE, SO WHAT WE HAVE IS OUR OWN SEARCH BOUND.
        //
        // `lo` is the highest rung the climb probed, and nothing above it was ever asked: the true
        // ceiling is at least `lo` and could be anywhere above. Publishing `lo` as Measured makes two
        // gateways with different real ceilings collapse onto the same number - ours - and the
        // difference between them reads as agreement. `bisect_ceiling` refuses precisely this case
        // ("publishing max_conc would report our own search bound as the gate's ceiling") and this
        // path is the same fact about the same kind of search, so it gets the same answer. The lower
        // bound travels as prose, and every rung that proved it travels as evidence.
        let detail = format!(
            "c={lo} still held the {}ms gate at the top of the search range; the sustained ceiling is at least {lo}, which is a bound this search chose rather than one the gateway showed us",
            SUSTAINED_P99_CEILING_US / 1_000
        );
        return Refined {
            rps: Measurement::absent_because(Absent::SearchExhausted, detail),
            // The detail lives once, on the rate; the concurrency mirrors its reason so one absence
            // cannot come back with two explanations.
            concurrency: Measurement::absent(Absent::SearchExhausted),
            points,
        };
    };
    let mut best = lo;
    let mut best_rps = top.gate_median.unwrap_or(top.median);
    {
        let (mut a, mut b) = (lo, hi);
        // Single windows are fine for SEARCHING - cheap and convergent - and are never what gets
        // published: `confirm_ceiling` re-measures the winner over a full `WINDOWS_PER_RUNG` and
        // refuses to publish a candidate it could not, which is the part that has to be right.
        while b - a > 1 {
            let mid = a + (b - a) / 2;
            let probed = p.probe(mid);
            if let Some(s) = probed.as_ref() {
                if let Some(r) = s.reading {
                    points.push(SustainedPoint {
                        concurrency: mid,
                        passed: sustained_gate_passes(r.p99_us, r.ok, r.fail),
                        rps: s.value,
                        p99_us: r.p99_us,
                        fail: Some(r.fail as i64),
                    });
                }
            }
            match probed {
                Some(s)
                    if s.reading
                        .is_some_and(|r| sustained_gate_passes(r.p99_us, r.ok, r.fail)) =>
                {
                    a = mid;
                    best = mid;
                    best_rps = s.value;
                }
                // A window that produced nothing narrows nothing: it is one fewer sample, not a
                // failure, and treating it as one would report the rig's bad luck as the gateway's
                // ceiling. Stop and publish what was actually proven.
                None => break,
                Some(_) => b = mid,
            }
        }
    }
    // A CEILING THE CLIMB ALREADY MEASURED NEEDS NO CONFIRMING.
    //
    // `confirm_ceiling` exists because a bisection lands on the highest concurrency that passed
    // ONCE, and one window is not a ceiling. But when the bisection never moved - the gate was still
    // holding at the top of the range, or the bracket was already adjacent - the answer is a rung the
    // CLIMB measured `WINDOWS_PER_RUNG` times, whose majority verdict and median are already in hand.
    // Re-probing it buys nothing and costs two load windows per cell, and those windows are now spent
    // inside `Throughput`, ahead of every later group rather than after them. That is not free: it
    // starved the streaming legs of an end-to-end run that had passed for seven commits, and the
    // first symptom was a TTFT that stopped being published rather than anything about throughput.
    if best == lo {
        return Refined {
            rps: Measurement::Measured(best_rps),
            concurrency: Measurement::Measured(lo),
            points,
        };
    }
    match confirm_ceiling(p, best, best_rps, &mut points) {
        Some((c, rps)) => Refined {
            rps: Measurement::Measured(rps),
            concurrency: Measurement::Measured(c),
            points,
        },
        // THE BISECTED CANDIDATE FAILED TO CONFIRM - THAT DOES NOT ERASE WHAT THE CLIMB PROVED.
        // `lo` held the gate by a majority of the climb's own windows; publishing 0 here would put
        // a zero beside a sweep whose evidence shows that rung holding, and would fold a rig
        // interruption during confirmation into a gateway verdict. Fall back to the climb's last
        // proven holder - the same answer the `best == lo` fast path publishes.
        None => Refined {
            rps: Measurement::Measured(top.gate_median.unwrap_or(top.median)),
            concurrency: Measurement::Measured(lo),
            points,
        },
    }
}

/// What the refinement proved, and every window it spent proving it.
struct Refined {
    rps: Measurement<f64>,
    concurrency: Measurement<u32>,
    points: Vec<SustainedPoint>,
}

/// Re-measure a proven ceiling `WINDOWS_PER_RUNG` times and publish the median of the windows that
/// HELD, stepping down by half whenever a majority does not - and publishing nothing at all when
/// fewer than `WINDOWS_PER_RUNG` windows could be judged, since a rate nobody re-measured is one
/// window's luck rather than a ceiling.
fn confirm_ceiling<P: crate::search::Probe>(
    p: &mut P,
    from: u32,
    first_rps: f64,
    points: &mut Vec<SustainedPoint>,
) -> Option<(u32, f64)> {
    let mut ceiling = from;
    let mut first = first_rps;
    for _ in 0..MAX_CEILING_STEPDOWNS {
        let mut repeats: Vec<(f64, bool)> = Vec::new();
        let mut held = 1usize;
        let mut judged = 1usize;
        for _ in 1..crate::search::WINDOWS_PER_RUNG {
            let Some(s) = p.probe(ceiling) else { continue };
            let Some(r) = s.reading else { continue };
            judged += 1;
            let passed = sustained_gate_passes(r.p99_us, r.ok, r.fail);
            points.push(SustainedPoint {
                concurrency: ceiling,
                passed,
                rps: s.value,
                p99_us: r.p99_us,
                fail: Some(r.fail as i64),
            });
            if passed {
                held += 1;
            }
            repeats.push((s.value, passed));
        }
        // A RUNG THAT PRODUCED FEWER WINDOWS THAN A CLIMB RUNG IS MEASURED WITH CANNOT PUBLISH.
        //
        // `judged` opens at 1 because the bisection's own window at this concurrency is a real vote,
        // and the bisection takes SINGLE windows by design - cheap and convergent, which is right for
        // a search but not for a published rate. When both repeats came back empty (a deadline, a
        // window that produced nothing) the majority test read 1 of 1 and published that one window's
        // uncorrected `value` as the cell's sustained throughput. One lucky window is exactly how a
        // sustained figure lands ABOVE the peak measured from the same sweep, the inversion class C6
        // catches downstream, and it is the one rate in this engine that was never a median of
        // `WINDOWS_PER_RUNG` windows. Every other rung on the board is measured that way; so is this
        // one now, or it is not published at all. The evidence shows which happened: the windows that
        // did land are in `points`, so a reader counts fewer than `WINDOWS_PER_RUNG` at a candidate
        // that was dropped and sees the climb's own rung published in its place.
        if judged >= crate::search::WINDOWS_PER_RUNG && held * 2 > judged {
            // THE SAME RULE THE SEPARATE SEARCH PUBLISHED BY, kept as one tested function rather
            // than re-implemented here: only windows that HELD the gate count toward the median,
            // and the window that proved the ceiling is the fallback when none of the repeats did.
            return Some((ceiling, sustained_median(first, &repeats)));
        }
        if judged < crate::search::WINDOWS_PER_RUNG {
            // THE RIG RAN SHORT, NOT THE GATEWAY. Stepping down here would walk the ceiling downward
            // on our own missing windows and charge the gateway for them; the caller falls back to
            // the climb's last proven holder, which is itself a `WINDOWS_PER_RUNG` median.
            return None;
        }
        if ceiling <= 1 {
            return None;
        }
        ceiling /= 2;
        // The stepped-down rung needs its own first window; the one above it proved nothing here.
        let stepped = p.probe(ceiling);
        if let Some(s) = stepped.as_ref() {
            if let Some(r) = s.reading {
                points.push(SustainedPoint {
                    concurrency: ceiling,
                    passed: sustained_gate_passes(r.p99_us, r.ok, r.fail),
                    rps: s.value,
                    p99_us: r.p99_us,
                    fail: Some(r.fail as i64),
                });
            }
        }
        match stepped {
            Some(s)
                if s.reading
                    .is_some_and(|r| sustained_gate_passes(r.p99_us, r.ok, r.fail)) =>
            {
                first = s.value;
            }
            _ => return None,
        }
    }
    None
}

/// Ceiling for the sustained-throughput gate. README's own definition: "highest sustained
/// requests/sec with p99 under [a latency ceiling]". Named for the artifact field it feeds
/// (`rps_sustained_20ms`) so the constant and the number it produces cannot drift apart in a later
/// edit that changes one without the other.
pub const SUSTAINED_P99_CEILING_US: u64 = 20_000;

/// The error-rate half of the same gate. README: "...and a <0.1% error rate". Not the throughput
/// sweep's own all-or-nothing clean-window bar (`fail == 0`, see `SweepProbe`): the sustained search
/// exists specifically to find where a gateway starts to strain, and demanding zero failures would
/// make a single dropped connection at an otherwise-healthy concurrency fail the WHOLE rung the same
/// way it fails a peak rung, collapsing "occasionally drops one connection in ten thousand" and
/// "cannot serve this concurrency at all" into the same verdict. The README's own number is the bar.
pub const SUSTAINED_MAX_FAIL_RATIO: f64 = 0.001;

/// How many times the sustained ceiling may step down when it fails confirmation.
///
/// Each step halves the concurrency, so this bounds the walk at a few doublings below the
/// bisection's answer - far enough to find a rung that genuinely holds, short enough that a cell
/// cannot spend its whole budget here.
const MAX_CEILING_STEPDOWNS: usize = 4;

/// One rung as the sustained-throughput GATE saw it, carrying the p99 and fail count behind its
/// pass/fail verdict.
///
/// Distinct from `search::ProbedPoint` because the two answer different questions about the same
/// window: the point says what the SEARCH made of it, this says what the GATE made of it. They now
/// describe the same windows - which is the whole change - but a reader comparing
/// `sweep_max_proxy` to `sweep_sustained_20ms` is comparing two readings of one sweep, and
/// collapsing them into one type would hide that there were ever two verdicts to reconcile.
#[derive(Debug, Clone, PartialEq)]
pub struct SustainedPoint {
    pub concurrency: u32,
    pub passed: bool,
    pub rps: f64,
    pub p99_us: Option<u64>,
    /// Failed requests in the window behind this rung, or `None` when no window carried a reading.
    ///
    /// `Option`, not `i64`, and that is the whole point: this was a bare `i64` whose only value for
    /// "no window reported" was 0, and `metric.rs` mapped it to `Measurement::Measured(0)`
    /// unconditionally - so a rung nothing was ever read from published `fail: 0` in
    /// `sweep_sustained_20ms`, a fabricated zero that reads as "it served this rate losing nothing".
    /// The board's central rule is that an absent measurement publishes null with a reason and is
    /// never substituted by a number; the type had no way to obey it.
    pub fail: Option<i64>,
}

/// Whether one window satisfies the sustained-throughput gate: p99 under the latency ceiling AND
/// the error rate under the README's bar.
///
/// A free function rather than logic inlined into a probe, so the gate's pass/fail boundary - the one
/// piece of judgement this whole metric turns on - can be unit-tested directly against fixed numbers,
/// the same reason `rigbound::is_rig_bound` is a free function rather than logic buried inside
/// `apply_peak_verdict`. A probe's own `probe()` drives a real subprocess load window and cannot be
/// exercised that way in this crate's unit tests (see `tests/end_to_end.rs`'s own note on why: under
/// `cargo test` the current exe is the test binary, not `otb`).
///
/// It is now applied to the SAME windows the throughput sweep took, via `sustained_gate`, rather
/// than to a second search's windows measured minutes later.
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

/// The rps published at a proven sustained ceiling: the median of the windows taken there that HELD
/// the gate, with the bisection's own window as the first of them.
///
/// Pure, and separated from the windowing on purpose. The rule is the whole of the fix and it has
/// three distinct branches that a live-load test cannot reach deterministically - a scattering
/// gateway, a window that fails the gate on re-measure, and a rig that returns nothing at all.
/// Driving it directly is what makes those branches testable at all; the surrounding function only
/// has to take the windows.
///
/// ONLY WINDOWS THAT HELD THE GATE COUNT, exactly as `search::measure_rung` excludes a failed window
/// from the peak's median. A window that blew the p99 ceiling sustained nothing, so folding its rate
/// into a SUSTAINED number would publish throughput the gate had just refused. Every window is still
/// recorded as evidence by the caller either way.
///
/// `first` is the fallback when no window survives that filter: the bisection already proved the
/// ceiling with it, so it is a measured reading, and returning it is what the engine published
/// before repeats existed.
pub(crate) fn sustained_median(first: f64, repeats: &[(f64, bool)]) -> f64 {
    let mut vals: Vec<f64> = std::iter::once(first)
        .chain(
            repeats
                .iter()
                .filter(|(_, passed)| *passed)
                .map(|(rps, _)| *rps),
        )
        .collect();
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    crate::search::nearest_rank_median(&vals).unwrap_or(first)
}

/// The mock's own delta interval, READ FROM THE VARIABLE THE MOCK READS.
///
/// Named here rather than inlined at the one comparison because this and `STREAM_STALL_MULTIPLIER`
/// together ARE the gate's definition - the README states it as "no stream stalls past 10x the mock's
/// pacing interval", and a reader who wants to know what "stalled" means on this board should find
/// one place that says so.
///
/// An older paragraph here claimed the interval was "a boot-time environment knob the engine cannot
/// observe over the wire, so this is a documented coupling rather than a derived value". The function
/// below reads that exact variable, so the claim was the direct opposite of the code beneath it, and
/// it also cited the README's threshold as 2x when both the README and `STREAM_STALL_MULTIPLIER` say
/// 10x. A reader cross-checking either statement would have concluded the wrong thing about which
/// side was stale.
///
/// This was a `20` hardcoded here to match `MOCK_STREAM_INTERVAL_MS`'s default over in the mock, with
/// a comment calling it a documented coupling. It is the same two-places-one-truth shape as the two
/// hand-rolled ladders: set the mock to a different pace and the engine keeps measuring stalls
/// against a cadence nothing is producing, silently, with every streaming rung judged by the wrong
/// bound. A "documented" coupling is one that is right until someone uses the knob.
///
/// So both sides now read the same variable and the default matches the mock's own. Nothing in the
/// field sets it today, which is exactly why the divergence would not have been noticed.
pub fn stream_pacing_interval_ms() -> u64 {
    std::env::var("MOCK_STREAM_INTERVAL_MS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        // A PACE OF ZERO IS NOT A PACE. A parsed 0 drove `stall_bound_us()` to 0, which makes EVERY
        // inter-frame gap - including a gap of nothing - count as a stall, so both stream metrics fail
        // on every rung of every cell and the board reads it as the gateway going quiet. The mock
        // refuses 0 too (`chunks.max(1)` and a sleep it never takes), so the two sides would not even
        // agree about what zero meant. Rejected in favour of the default, which is what an unset
        // variable already gets.
        .filter(|v| *v > 0)
        .unwrap_or(20)
}
/// The stall bound as a multiple of the mock's pacing interval: a gap past this is a stream that
/// WENT QUIET, not one that wobbled off the mock's clock.
///
/// It was 2, and 2x a 20ms pace is a 40ms budget - a bar that mostly measured whether the gateway
/// could keep to the mock's own clock under concurrency, which is the added_gap percentiles' job
/// (they quantify pacing fidelity to the microsecond, per leg, and publish it). On the 2026-07-28
/// board that budget failed nearly every gateway at every rung (streams_sustained on 6 of 16 served
/// cells, cpu_fps on 1), so the search returned absence across the board and the metric measured
/// nothing at all. This metric's question is DELIVERY: every expected frame arrives
/// (`STREAM_MIN_DELIVERY_RATIO = 1.0`, deliberate, unchanged) and no lane goes quiet mid-stream.
/// 10x the pace (200ms at the default 20ms) is a bound a reader would recognise as "the stream
/// stalled", while scheduler jitter and GC pauses well past the mock's clock stay the gap metric's
/// finding rather than this gate's.
pub const STREAM_STALL_MULTIPLIER: u64 = 10;

/// Fraction of expected frames that must arrive, and the share of streams that may fail, for a
/// concurrency to hold the streams-sustained gate.
///
/// EVERY FRAME. A proxy that drops a frame has dropped a user's token, and there is no concurrency
/// at which that is the gateway succeeding - so the sustained ceiling is the last rung before
/// anything is lost, which is exactly the number this metric is for. It was 0.999, which sounds
/// tight and is not: at c=256 and a 64-frame budget it waves through 16 lost frames per window, and
/// the loss it admits is invisible in the published rate.
///
/// The bound stays reachable because the rungs below the gateway's limit really are perfect: 1169 of
/// the 1314 passing rungs in the 2026-07-28 field run delivered every expected frame. The 145 that
/// did not are the point - they are where a gateway started losing tokens, and the gate now stops
/// there instead of climbing past it.
pub const STREAM_MIN_DELIVERY_RATIO: f64 = 1.0;
pub const STREAM_MAX_ERROR_RATIO: f64 = 0.001;

/// What one window of `concurrency` concurrent streams did.
///
/// Counts rather than rates, plus the wall clock, so every published number below is derived here
/// once from the same window. Splitting "how many frames" from "how long" across two windows is the
/// two-populations defect `metric.rs`'s module doc names.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamWindow {
    pub concurrency: u32,
    /// Streams opened, and of those, the ones that never became a readable event stream at all
    /// (no connection, a non-2xx, a malformed head, or a peer that answered with something that is
    /// not an event stream). A stream that opened and then delivered short is NOT an error: it is a
    /// delivery shortfall, and the two are separate halves of the README's gate.
    pub streams: u64,
    pub errored: u64,
    /// EVERY SSE event dispatched, across every lane. What `fps` is computed from, and what anything
    /// asking "did this stream at all" wants.
    ///
    /// MAY EXCEED `expected_frames`, and that is not a surplus of tokens. Each lane reads until its
    /// CONTENT budget is delivered (`http::SseBudget::Content`), so a gateway that inserts pings or
    /// re-frames a translated stream spends more events than the mock's own layout would - which is
    /// precisely the case the event-budgeted read used to publish as a delivery shortfall.
    /// `expected_frames` stays the mock-shaped budget, because what it answers ("did it stream at
    /// all", and the scale fps is read against) is unchanged by the gateway's framing style.
    pub frames: u64,
    pub expected_frames: u64,
    /// Of `frames`, the ones that carried MODEL OUTPUT, as the request's own dialect classifies them
    /// (`ingress::Dialect::sse_event_is_content`), and the most a full budget could have carried.
    ///
    /// Ledger RIG-11, and the reason there are two pairs of counts here rather than one. The delivery
    /// gate is the one number that must not count scaffolding: openai spends 3 events on framing and
    /// anthropic 5, so a lane could satisfy 1 (openai) or 2 (anthropic) frames of its budget before
    /// a single token arrived, and the two dialects differed from each other by exactly two on a
    /// ratio that is compared against a fixed bound of 1.0. `frames`/`expected_frames` are kept
    /// beside them unchanged, because fps and "did it stream" legitimately want every event.
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

    /// The share of expected MODEL OUTPUT that arrived - the numerator counts content frames, not
    /// every SSE event.
    ///
    /// This used to be `frames / expected_frames`, which credited a lane for `message_start` and
    /// `content_block_start` exactly as it credited it for a token. Against a bound of 1.0 that is
    /// not a rounding difference: it is 1 (openai) or 2 (anthropic) frames of every lane's budget
    /// satisfied by scaffolding, and a two-frame constant offset BETWEEN the two dialects on a
    /// number the board compares across them.
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
}

/// Whether one window holds the README's streams-sustained gate: EVERY expected content frame
/// arrived, no lane stalled past `STREAM_STALL_MULTIPLIER` times the mock's pace, and almost no
/// stream failed outright.
///
/// A free function over plain counts, like `sustained_gate_passes`, so the one piece of judgement the
/// whole search turns on can be pinned directly against fixed numbers rather than only through a
/// window that needs a live mock behind it.
pub fn streams_gate_passes(w: &StreamWindow) -> bool {
    streams_gate_verdict(w).is_none()
}

/// WHY one window failed the gate, or `None` when it held. The clause that tripped is named with the
/// counts that tripped it, so a rung that fails publishes evidence a reader can weigh instead of a
/// bare `passed: false` - "a gateway failing at every rung must publish a reason, never a bare
/// absence" is the defect class this exists for.
pub fn streams_gate_verdict(w: &StreamWindow) -> Option<String> {
    // A window that opened no stream, or expected no frame, has not PASSED anything - it measured
    // nothing, and a ratio computed from zero must never read as a clean window by accident of
    // floating-point division.
    if w.streams == 0 {
        return Some("the window opened no stream, so it measured nothing".to_string());
    }
    // Expected CONTENT frames, because that is the denominator the delivery clause below divides by.
    // A dialect whose whole budget is prelude expects no tokens at all, and a ratio computed from
    // zero must never read as a clean window by accident of floating-point division.
    if w.expected_content_frames == 0 {
        return Some(format!(
            "the window opened {} stream(s) but expected no content frames, so it measured nothing",
            w.streams
        ));
    }
    let mut why = Vec::new();
    if w.delivery_ratio() < STREAM_MIN_DELIVERY_RATIO {
        // Content frames on BOTH sides of the "of", and said in words, because the raw event count
        // beside it is larger and a reader comparing the two would otherwise think frames went
        // missing that never carried a token in the first place.
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

/// How many gaps in one lane's frame arrivals exceeded the stall bound.
///
/// GAPS, not the first frame's offset: time to first token is a latency the `Streaming` group already
/// publishes as a difference, and charging it here would make every stream stall on a gateway that is
/// merely slow to start rather than one that goes quiet mid-stream, which is what the README's rule
/// is about.
fn stalls_in(offsets: &[u64]) -> u64 {
    offsets
        .windows(2)
        .filter(|w| w[1].saturating_sub(w[0]) > stall_bound_us())
        .count() as u64
}

/// Whether one lane's outcome is a stream that never existed, as opposed to one that ran short.
///
/// ZERO FRAMES IS AN ERROR, NOT A SHORTFALL, and that is the case worth stating: the README's own
/// rule is that "a gateway that answers 200 but buffers the stream (never frames)" is recorded as not
/// having streamed. Such a peer answers a perfectly valid 200 and simply never sends an event, so
/// every other signal here reads clean; folding it into the delivery ratio instead would let a
/// gateway that streams NOTHING be averaged away against lanes that did, and at high concurrency a
/// handful of buffering lanes would vanish entirely.
fn stream_errored(o: &crate::http::SseOutcome) -> bool {
    // THE RIG RUNNING OUT IS NOT AN ERRORED STREAM. A lane that could not get a source port never
    // asked the gateway anything, and counting it here published our own exhaustion as the gateway's
    // stream ceiling. `stream_window` discards the whole window instead - see `rig_exhausted_in`.
    if matches!(o.end, crate::http::SseEnd::RigExhausted(_)) {
        return false;
    }
    // NEITHER IS A REQUEST WE REFUSED TO SEND. The gateway was never asked, so it cannot have
    // errored; `stream_window` discards the whole window and says so loudly, exactly as it does for
    // exhaustion. Charging a manifest defect of ours to the gateway's stream error rate is the
    // attribution inversion this file refuses everywhere else.
    if matches!(o.end, crate::http::SseEnd::RigRefused(_)) {
        return false;
    }
    if !o.status.is_some_and(|s| (200..300).contains(&s)) {
        return true;
    }
    if o.frame_offsets_us.is_empty() {
        return true;
    }
    // `EventCeilingReached` is deliberately NOT here. A lane that spent the whole event ceiling
    // without delivering its content budget streamed perfectly well by every structural measure - it
    // just did not deliver, which is the delivery clause's finding and shows up in the ratio. Calling
    // it an errored stream would double-count one shortfall as two failures and blur the two halves
    // of the README's gate that this function exists to keep apart.
    matches!(
        o.end,
        crate::http::SseEnd::ConnectionFailed(_)
            | crate::http::SseEnd::Malformed(_)
            | crate::http::SseEnd::NotAnEventStream(_)
    )
}

/// Drive `concurrency` concurrent streams against `addr` and read each one to the frame budget.
///
/// `None` means the window never ran at the requested concurrency - the OS refused a lane - which is
/// a RIG limit exactly as `SweepProbe`'s `spawn_failed` is: nothing about the gateway was learned, so
/// the search must stop rather than read the shortfall as a turnover.
///
/// IN-PROCESS THREADS, unlike `load_window`'s pinned child. Two reasons, and the second is the one
/// that matters: a stream lane spends its life asleep between the mock's 20 ms deltas rather than
/// saturating a core, so it does not contend for the orchestrator's CPU the way the request generator
/// would; and the mock-ceiling reference below is taken with THIS SAME function against the mock, so
/// whatever the instrument's own overhead is, it is charged identically to both legs of the
/// comparison. What this does NOT get is the generator's core pinning, and at concurrencies high
/// enough to saturate the box that is a real limitation on the absolute frames/sec - which is exactly
/// why the mock-bound guardrail is applied to these numbers rather than them being published bare.
///
/// `dialect` is the wire the request speaks, and it is not optional here: without it the delivery
/// ratio counts protocol scaffolding as delivered tokens (ledger RIG-11), which is the one place the
/// distinction changes a published verdict.
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
    // AND THE READ IS BUDGETED IN THOSE CONTENT FRAMES, not in events - which is what makes the
    // denominator above safe to compare against.
    //
    // `content_budget` is a CONSTANT, computed from the MOCK's layout, while the numerator is
    // measured on the GATEWAY's stream. Reading to a fixed number of EVENTS made every non-content
    // event the gateway adds beyond the mock's own prelude displace exactly one content frame, so
    // the ratio landed under `STREAM_MIN_DELIVERY_RATIO` (1.0, deliberate) at EVERY rung on a
    // gateway that lost nothing: anthropic's real protocol sends `ping`s, a translation cell has the
    // gateway re-emitting the stream with ITS own framing rather than the mock's, and a keepalive
    // does the same. Same mistake `STREAM_STALL_MULTIPLIER` (2 -> 10) fixed on the other clause of
    // this gate - a bound calibrated on the mock, applied to gateways that do not share its
    // behaviour.
    //
    // Reading until the tokens arrive asks the question the metric is actually asking. A gateway
    // that inserts framing simply spends more events on the same delivery. Bounded by
    // `STREAM_EVENT_CEILING` (and by `STREAM_TIMEOUT` as before), and a lane that hits the ceiling
    // short of its content is a REAL shortfall that still fails the gate on the count.
    let lane_budget = crate::http::SseBudget::Content {
        frames: content_budget,
        event_ceiling: crate::metric::STREAM_EVENT_CEILING,
    };

    // ONE TOKIO TASK PER LANE, NOT ONE OS THREAD.
    //
    // A thread per lane is what capped the concurrent-stream searches far below the throughput
    // searches: 65536 threads is scheduler thrashing, not a bigger gateway, and a field run that
    // tried it sat at a 1-minute load average over 24,000 and never converged. That cap was OUR
    // limit arriving on the board as the gateway's - 15 cells of the 2026-07-28 run published no
    // cpu_fps at all because the search "was still climbing" at the point the harness gave up.
    //
    // The lane body is `post_json_sse_async`, which feeds the SAME `SseReader` and sends the same
    // bytes as the blocking lane; a differential test drives both against one peer and asserts they
    // decode identically. So this changes who owns the waiting and nothing about what is measured.
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
            // The clock starts once every lane exists, exactly as `gen.rs::run` does: the ramp of
            // creating them must not land in the denominator, or fps() is depressed hardest at exactly
            // the high rungs the search is climbing toward.
            let started = std::time::Instant::now();
            let mut out = Vec::with_capacity(lanes.len());
            let mut panicked = 0usize;
            for l in lanes {
                // A PANICKED LANE IS A HARNESS FAULT, not a gateway failure, and it is also not a
                // stream: it is counted in neither column, because attributing our own defect to the
                // gateway's error rate is the exact inversion this engine refuses everywhere else.
                //
                // COUNTED, THOUGH. Dropping it silently is what made a four-hour run undiagnosable: a
                // lane that panicked left no number, no message, and no trace anywhere in the artifact
                // or the log, so the only visible symptom was a metric that took 0.0s and published an
                // absence. "Not the gateway's fault" is a reason to keep it out of the gateway's error
                // rate, never a reason to lose it.
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
        frames: 0,
        expected_frames: 0,
        content_frames: 0,
        expected_content_frames: 0,
        stalls: 0,
        elapsed_s,
    };
    // A REQUEST WE REFUSED TO SEND MEASURED NOTHING AT ALL, and unlike exhaustion it is not even a
    // resource limit - it is a defect in a first-party manifest that would have put a smuggled header
    // on the wire. Loud, unmeasured, never the gateway's: see `http::SseEnd::RigRefused`.
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
    // A WINDOW THAT RAN OUT OF RIG NEVER RAN AT THE CONCURRENCY IT CLAIMS, exactly as a loadgen
    // window with `rig_refused` did not. Unmeasured, not a failing rung: the alternative is
    // publishing this host's ephemeral port range as the gateway's concurrent-stream ceiling.
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
    Some(w)
}

/// May a stream ceiling be PUBLISHED from these windows? Pure, so the rule can be tested without a
/// socket - the same reason `window_refusal` and `cpu_fps_result_from_search` are separate.
///
/// TWO CONDITIONS, and the stream path used to carry only the second. A window that could not run is
/// skipped without incrementing `total`, so two absent repeats left the count at 1 of 1 and `1 * 2 > 1`
/// published the bisection's single unrepeated window as a confirmed ceiling. Both absent paths -
/// `SseEnd::RigExhausted` when the host runs out of ephemeral ports or descriptors, and a panicked
/// lane - are reachable exactly at the top rung, so the shrinking denominator was likeliest precisely
/// where the published number matters most.
///
/// `confirm_ceiling` refuses the same input on the throughput side, and its comment says why: one
/// lucky window is how a sustained figure lands ABOVE the peak measured from the same sweep, which is
/// the inversion class C6 catches downstream. Every other rate on this board is a median of
/// `WINDOWS_PER_RUNG` windows; so is this one, or it is not published.
fn stream_ceiling_confirmed(total: usize, held: usize) -> bool {
    total >= crate::search::WINDOWS_PER_RUNG && held * 2 > total
}

/// Why a stream window must be DISCARDED rather than published, or `None` when it may stand.
///
/// Pure, and separate from the window that feeds it, for the reason `apply_peak_verdict` and
/// `cpu_fps_result_from_search` are separate from theirs: the rule is plain arithmetic, while the
/// only part that needs a socket is opening one. Deciding it inline left it unreachable from any
/// test - and the rule it encodes had already cost a four-hour run its cpu_fps on every streamable
/// cell, silently, which is precisely the defect class this codebase treats as its worst.
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

/// One rung a stream search actually probed, carrying the counts behind its verdict.
///
/// Its own type rather than `search::ProbedPoint` for the reason `SustainedPoint` is: the generic
/// point carries a pass/fail and a value because that is all a generic gate search can observe, and
/// widening it with delivery counts that two of its four callers never fill would make its shape a
/// lie about what a search knows.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamPoint {
    pub concurrency: u32,
    pub passed: bool,
    pub fps: f64,
    pub frames: u64,
    pub expected_frames: u64,
    /// The two counts the DELIVERY clause is actually computed from. `frames`/`expected_frames` are
    /// not: they count every SSE event, so a rung published with only those could not be re-derived
    /// by a reader - and now that a lane reads to its content budget rather than to a fixed event
    /// count, `frames` can legitimately come in ABOVE `expected_frames` on a gateway that inserts
    /// framing, which reads as nonsense without the content pair beside it.
    pub content_frames: u64,
    pub expected_content_frames: u64,
    pub streams: u64,
    pub errored: u64,
    pub stalls: u64,
    /// WHY the gate failed, from `streams_gate_verdict`, when it did. A failing rung publishes the
    /// clause that tripped with the counts that tripped it, so "no rung passed" is never a bare
    /// absence a reader has to re-derive from the raw counts.
    pub why: Option<String>,
}

impl StreamPoint {
    /// The published rung, self-describing: the concurrency, the rate, the verdict, and every count
    /// the verdict was computed from, so a reader can re-derive the pass/fail rather than trust it.
    /// `sweep_streams`/`sweep_cpu_fps` are `Vec<serde_json::Value>` on the wire (record.rs never
    /// pinned a shape against a real artifact, because none has ever carried one), so this is where
    /// the shape is decided, in one place, rather than at each of the two searches.
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

/// Drives concurrent streams at one concurrency for the cpu-bound frames/sec PEAK.
///
/// A different gate from the one above, deliberately. The sustained search asks "does this
/// concurrency still deliver a clean stream", which is a quality bar; the peak search asks "how many
/// frames per second can the box carry through this gateway", and holding a rung to the delivery bar
/// there would refuse to look at exactly the region where the ceiling lives. So a rung counts here
/// iff it produced frames and no stream failed outright - the same clean-window shape `SweepProbe`
/// uses for the throughput peak (`fail == 0 && ok > 0`), read in frames instead of requests.
struct StreamFpsProbe {
    addr: SocketAddr,
    path: String,
    body: String,
    headers: Vec<(String, String)>,
    dialect: Dialect,
    points: Vec<StreamPoint>,
}

impl Probe for StreamFpsProbe {
    fn probe(&mut self, concurrency: u32) -> Option<Sample> {
        let w = stream_window(
            self.addr,
            &self.path,
            &self.body,
            &self.headers,
            self.dialect,
            concurrency,
        )?;
        // THE SAME GATE THE OTHER STREAM SEARCH USES. This was `errored == 0 && frames > 0`, which
        // passes a window that delivered ONE frame of an expected sixty-four, and the frames/sec
        // computed from it was published as "the most frames a second this box carries through the
        // gateway". gomodel openai>gemini did exactly that in the 2026-07-28 run: 1/64 delivered,
        // 1.56%, rung passed. Two searches over the same windows must not disagree about what a
        // healthy window is.
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
/// `bisect_ceiling`, not `saturation_plateau`: this is a monotone pass/fail gate in concurrency, exactly like
/// `sweep_sustained_cell`. Once enough concurrent streams are in flight that frames start arriving
/// late or short, adding more does not bring them back.
pub fn sweep_streams_cell(cfg: &RunConfig, id: &CellId, lo: u32, hi: u32) -> CellStreams {
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
        // `bisect_ceiling`'s own MEASURED "nothing sustains this gate": there is no rung to read a
        // rate back from, so the rate is a real zero rather than a lookup that would miss.
        Some(0) => CellStreams {
            concurrency: Measurement::Measured(0),
            fps: Measurement::Measured(0.0),
            points: p.points,
        },
        // CONFIRMED, for the same reason the sustained ceiling is: `bisect_ceiling` walks up until ONE
        // window fails, so it lands exactly on the boundary - the highest concurrency that passed
        // once. Re-measuring the sustained ceilings of the 2026-07-28 run found 9 of 48 held their
        // gate in only 1 of 3 windows. This gate is STRICTER (every expected frame must arrive, no
        // stalls), so a boundary rung here is if anything more likely to be marginal, and nothing was
        // re-measuring it at all.
        Some(c) => match p.points.iter().find(|pt| pt.concurrency == c).map(|pt| pt.fps) {
            Some(v) => {
                let mut ceiling = c;
                let mut first_fps = v;
                let mut winner: Option<(u32, f64)> = None;
                for _ in 0..MAX_CEILING_STEPDOWNS {
                    let mut held = 1usize; // the bisection's own winning window is a real vote
                    let mut total = 1usize;
                    let mut rates = vec![first_fps];
                    for _ in 1..crate::search::WINDOWS_PER_RUNG {
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
                    // THE SAME TWO-PART RULE `confirm_ceiling` USES, not just its majority half.
                    //
                    // A window that could not RUN takes the `continue` above without incrementing
                    // `total`, so two absent repeats left this at 1 of 1 - and `1 * 2 > 1` is true, so
                    // the bisection's single unrepeated window was published as a confirmed ceiling
                    // along with its own raw fps. Both absent paths are reachable exactly here, at the
                    // top rung: `SseEnd::RigExhausted` when the host runs out of ephemeral ports or
                    // descriptors, and a panicked lane. So the shrinking denominator was likeliest
                    // precisely where the number matters most.
                    //
                    // `confirm_ceiling` refuses this input on the throughput side and says why: one
                    // lucky window is how a sustained figure lands ABOVE the peak from the same sweep.
                    // The stream copy carried the majority test and dropped the minimum-window half,
                    // and the comment claiming it "runs the same majority rule" was true only of the
                    // half that survived. A rate here is a median of WINDOWS_PER_RUNG windows or it is
                    // not published.
                    if stream_ceiling_confirmed(total, held) {
                        // The published rate is the median of the windows that HELD, matching how
                        // every other repeated measurement in this engine reports.
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
                        break;
                    }
                    let next = ceiling / 2;
                    if next < lo.max(1) || next == ceiling {
                        break;
                    }
                    eprintln!(
                        "streams: c={ceiling} held the gate in only {held} of {total} windows - stepping down to c={next}"
                    );
                    ceiling = next;
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
                                break;
                            }
                            first_fps = w.fps();
                        }
                        None => break,
                    }
                }
                match winner {
                    Some((conc, fps)) => CellStreams {
                        concurrency: Measurement::Measured(conc),
                        fps: Measurement::Measured(fps),
                        points: p.points,
                    },
                    None => CellStreams {
                        concurrency: Measurement::absent(Absent::NotMeasured),
                        fps: Measurement::absent_because(
                            Absent::NotMeasured,
                            format!(
                                "the bisection proved c={c}, but that concurrency did not hold the stream gate \
                                 on re-measurement and stepping down found none that did within {MAX_CEILING_STEPDOWNS} attempts"
                            ),
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

/// Find the frames/sec ceiling: how many frames a second the box can carry through this gateway.
///
/// `saturation_plateau`, not `bisect_ceiling`, because this is a CEILING metric - frames/sec climbs
/// with added streams while there is CPU left to schedule them, and then flattens once there is not.
/// It flattens rather than falling: the same plateau shape throughput has, and the same reason
/// (past saturation the extra streams queue instead of being served), which is why it takes the same
/// search rather than one of its own. The mock's 20 ms pace means a single stream is
/// nowhere near the box's limit, so the number this finds is bounded by the machine rather than by
/// the protocol, which is what "cpu-bound" names. The engine cannot unpace the mock (its interval is
/// a boot-time knob), so the ceiling is reached by adding streams, not by asking for faster ones.
pub fn sweep_cpu_fps_cell(cfg: &RunConfig, id: &CellId, lo: u32, hi: u32) -> CellStreams {
    let Some(t) = stream_target(cfg, id) else {
        return CellStreams {
            concurrency: Measurement::absent(Absent::Untestable),
            fps: Measurement::absent(Absent::Untestable),
            points: Vec::new(),
        };
    };
    let mut p = StreamFpsProbe {
        addr: cfg.gateway_addr,
        path: t.path,
        body: t.body,
        headers: t.headers,
        dialect: t.dialect,
        points: Vec::new(),
    };
    // No start argument: `saturation_plateau` always climbs from the floor. A start derived from the
    // range made the ladder arbitrary and made a WIDER range open with a HIGHER first probe, which is
    // how a 1..65536 run began by asking for 32768 concurrent connections.
    // Same bound as the sustained search above, and for the same reason: these two climb the same
    // held-open streams, so a ceiling that is right for one cannot be wrong for the other.
    let hi = hi.min(stream_connection_ceiling());
    let lo = lo.min(hi);
    let r = search::saturation_plateau(&mut p, lo, hi);
    cpu_fps_result_from_search(&r.peak, p.points)
}

/// Turn what the fps search found into the cell's published pair. PURE, and separate from the search
/// that feeds it, for the reason `apply_peak_verdict` is separate from its judge: everything this
/// decides is plain data, while the only part that needs a socket is building the probe. Deciding it
/// inline left the rule untestable - a round-2 audit deleted the `NotMeasured` key below and all 60
/// `run::` tests plus all 11 search-property tests stayed green, which is the whole defect class this
/// codebase treats as its worst.
///
/// A MEASURED FAILURE IS A ZERO, NOT AN ABSENCE (the board's own rule, stated by its owner: 0 is a
/// number, n/a is not). When the load was really offered (rungs exist) and every rung failed the
/// gate, the gateway carried zero clean frames/sec - the same semantics `sweep_streams_cell` already
/// publishes for a floor that fails. Each failed rung's `why` travels in the sweep, so the zero
/// arrives with its evidence.
///
/// THE `NotMeasured` KEY IS LOad-BEARING. Absence remains for every case where the question was never
/// posed or the answer is a lower bound - untestable, rig-limited, an exhausted search, an
/// interrupted probe - and an interruption is exactly the shape that reaches here with rungs present
/// and all of them failed. Without the key, a rig that aborted mid-climb publishes as the gateway's
/// honestly-measured zero.
fn cpu_fps_result_from_search(
    peak: &Measurement<crate::search::PeakPoint>,
    points: Vec<StreamPoint>,
) -> CellStreams {
    if let Some(pt) = peak.value() {
        return CellStreams {
            concurrency: Measurement::Measured(pt.concurrency),
            fps: Measurement::Measured(pt.value),
            points,
        };
    }
    let offered = !points.is_empty();
    let gate_failed_everywhere = offered
        && peak.reason() == Some(&Absent::NotMeasured)
        && points.iter().all(|pt| !pt.passed);
    if gate_failed_everywhere {
        return CellStreams {
            concurrency: Measurement::Measured(0),
            fps: Measurement::Measured(0.0),
            points,
        };
    }
    CellStreams {
        concurrency: Measurement::absent(peak.reason().cloned().unwrap_or(Absent::NotMeasured)),
        fps: match (peak.reason().cloned(), peak.detail()) {
            (Some(reason), Some(detail)) => Measurement::absent_because(reason, detail),
            (Some(reason), None) => Measurement::absent(reason),
            (None, _) => Measurement::absent(Absent::NotMeasured),
        },
        points,
    }
}

/// The MOCK's own frames/sec at one concurrency, driven straight at it.
///
/// The streaming analogue of `suite::rig_ceiling`, and it takes the reference AT THE OPERATING POINT
/// the gateway's own number was taken at, for the reason `rigbound.rs`'s header gives: the rig is not
/// equally fast at every concurrency, so a reference from the top of the range would systematically
/// understate how close the gateway came to it.
///
/// A single window, not a search: a point measurement makes no turnover claim, so there is nothing
/// for a flanking check to refuse.
/// Takes the mock's address, model and token as arguments rather than a `RunConfig`. `rig_ceiling`
/// has to build a mock-facing config because the request generator's search plumbing reads one, and
/// every gateway-shaped field in it then has to be individually blanked with a comment explaining
/// why, a shape that exists to be defused. A stream window takes where to send and what to send, so
/// this says the same thing in three arguments with nothing to blank out.
pub fn stream_fps_at(
    mock_addr: SocketAddr,
    model: &str,
    auth: &str,
    dialect: Dialect,
    concurrency: u32,
) -> Measurement<f64> {
    let path = dialect.mock_direct_path(model);
    let body = dialect.stream_body(model);
    // The MOCK's own auth shape, with no gateway routing headers: those select an upstream INSIDE a
    // gateway and mean nothing here, exactly as `mock_healthy` already reasons.
    let headers = dialect.auth_headers(auth);

    // THE MEDIAN OF THE SAME NUMBER OF WINDOWS THE OBSERVATION GETS.
    //
    // This took ONE window, and its fps became the ceiling that judges a number the search built from
    // the median of `WINDOWS_PER_RUNG` windows - a bar chosen by a single sample, policing a figure
    // deliberately made "resistant to one unlucky window". When the single reference window came in
    // low, the observation cleared it by more than `IMPOSSIBLE_FACTOR` and the gateway's real,
    // repeatedly-measured number was thrown away as unvouchable.
    //
    // That is not hypothetical. The mock streams 64 frames at 20ms, so a lane carries ~50 frames/sec
    // and c=469 has a theoretical ceiling near 23,450. Bifrost was measured at 21,404 there - 91% of
    // theory, entirely plausible - which means the reference must have come back under 14,269, about
    // 61% of theory. Seven gateway/metric pairs on the 2026-07-29 board published nothing for this
    // reason. The gateway cannot beat the mock it forwards to, so an overshoot was always evidence
    // about this measurement rather than about the gateway; making it as strong as the number it
    // polices is the fix, not widening the factor that catches it.
    // AND LET THE BOX SETTLE FIRST. The reference is taken at the WINNING concurrency, which is only
    // known once the search has finished - so it is measured immediately after a ladder that just
    // drove thousands of concurrent streams through this same host. Sockets are still draining and
    // the CPU is still hot, and every one of those depresses the direct-to-mock number that is about
    // to be used as a ceiling. The median above defends against ONE unlucky window; it cannot defend
    // against a box that is uniformly busy, which is what this pause is for.
    //
    // Short on purpose. It runs twice per served streaming cell, so it is minutes across a field run,
    // and it does not need to outlast TIME_WAIT to be worth having - the queues and the run queue
    // drain in far less than that, and those are what move this number.
    std::thread::sleep(std::time::Duration::from_secs(2));

    let mut clean: Vec<f64> = Vec::with_capacity(crate::search::WINDOWS_PER_RUNG);
    let mut why: Option<String> = None;
    for _ in 0..crate::search::WINDOWS_PER_RUNG {
        match stream_window(mock_addr, &path, &body, &headers, dialect, concurrency) {
            // The reference must be a CLEAN window or it is not a ceiling: a reference taken while
            // the mock itself was dropping streams would be an understated bar, and a gateway
            // measured against it would read as rig-bound when it was not.
            // WHAT THE REFERENCE WINDOW ACTUALLY SAW, on one line per window.
            //
            // A reference that comes back low is indistinguishable, from the artifact alone, between
            // "the window was slow" and "the window read fewer frames" - `fps` is frames/elapsed and
            // collapses both. On the 2026-07-29 bifrost run the reference landed at exactly 50% of
            // theory in both cells while the gateway leg measured 86%, and there was no way to tell
            // which half of that fraction was wrong. Locally the same window at the same concurrency
            // is stable at 83-89% of theory with exactly 64 frames per stream, so whatever depresses
            // it is specific to the bench box and cannot be reasoned about from here.
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
    /// Every metric the engine took on this cell, keyed by the artifact field it fills. `None` for a
    /// cell that was not served: there is nothing to measure, and an empty map would read as
    /// "measured nothing" rather than "never asked".
    pub metrics: Option<std::collections::BTreeMap<&'static str, Measurement<f64>>>,
    /// The evidence behind those scalars: the rungs the throughput search probed and the resident
    /// memory readings taken across the load window. `None` alongside `metrics` for a cell that was
    /// never measured, and empty for one that was measured but produced no series.
    pub series: Option<crate::metric::Series>,
    /// SECONDS PER METRIC GROUP, so a slow run can be diagnosed offline instead of re-run with a
    /// stopwatch. Keyed by the group's own name (`throughput`, `streaming`, `memory`, ...).
    ///
    /// A total is not an answer: "this cell took thirteen minutes" cannot distinguish the TTFT
    /// sample set from a stream ladder that climbed to a higher rung from a gateway that simply got
    /// slower, and those have completely different responses. Published per cell so the question
    /// "what would we save by halving the TTFT samples" is arithmetic on the artifact.
    ///
    /// `None` for a cell that was never measured, matching `metrics` and `series`.
    pub timings_s: Option<std::collections::BTreeMap<&'static str, f64>>,
    /// Whether the gateway was PROVEN to have emitted this cell's egress dialect upstream, and the
    /// evidence behind that verdict. See `reverify.rs`: this is an anti-false-positive guard rather
    /// than a measurement, which is why it is a plain tri-state beside the metrics rather than one of
    /// them. `Default` (both `None`) for a cell that was never served, where there is nothing to
    /// re-verify.
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

/// The same walk, HANDING EACH CELL OVER AS IT FINISHES rather than at the end.
///
/// `run_grid_with` returns a `Vec`, which means the whole grid must finish before its caller sees
/// anything. `suite.rs` iterates that `Vec` and flushes a snapshot at every egress-column boundary,
/// under a comment promising that "a run interrupted partway through must not lose every cell it
/// already measured" - a promise the shape of the call made impossible to keep. Busbar measured 16
/// of 36 cells across four hours; the loop never started, no checkpoint was ever written, and the
/// box was torn down with every one of those measurements still in memory. Not one number survived.
///
/// So the walk pushes, and the caller decides when that is worth persisting. `run_grid_with` stays
/// as the collecting wrapper because most tests genuinely do want the whole grid at once.
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
            // A CELL THE MANIFEST DECLARES OUT OF SCOPE, OR THE RIG CANNOT POSE, IS NEVER PROBED.
            // Checked before `probe_cell` runs at all: sending the request and then discarding its
            // status is not the same as never sending it, because a global auth gate or rate limiter
            // still answers with a real status that has nothing to do with this specific pairing -
            // see `RunConfig::matrix`'s own doc for why that status must never be graded.
            if crate::manifest::is_untestable_cell(&cfg.untestable_cells, ing.as_str(), eg.as_str())
            {
                let note = if cfg.untestable_note.is_empty() {
                    "the rig cannot pose this pairing".to_string()
                } else {
                    cfg.untestable_note.clone()
                };
                // ONE LINE PER CELL, TO STDERR, AS IT IS DECIDED - not buffered until the grid
                // finishes. A box that dies mid-run leaves this trail in .run.log for whatever it
                // reached, and a live run can be tailed for real progress instead of going dark
                // until the sentinel lands.
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
            // THE RIG IS RE-CONFIRMED FOR EVERY CELL, not once for the whole grid.
            //
            // `mock_healthy` is what lets `persistent_transient_verdict` answer NotVerified: when the
            // rig cannot vouch for itself, nothing observed is attributable to the gateway. Reading
            // it once before a grid that runs for hours defeated exactly that. busbar's 36 cells take
            // about ninety minutes; a mock that degraded at cell five left every cell after it graded
            // as though the rig were confirmed fine, so its failures became the gateway's verdict -
            // the same inversion as counting our own port exhaustion as a refusal, just with a
            // longer fuse.
            //
            // One request per cell against a mock that is answering hundreds of thousands. The cost
            // is not worth thinking about; the wrong verdict is.
            let healthy = mock_healthy(cfg);
            if !healthy {
                eprintln!("[cell {done}/{total}] {id}: the mock did not answer its own health check - nothing observed here is attributable to the gateway");
            }
            let mut served = probe_cell(cfg, &id, healthy);
            // A GATEWAY THAT DIED TAKES THE REST OF THE GRID WITH IT, UNLESS SOMETHING RESTARTS IT.
            //
            // The retry budget inside `probe_cell` outlasts a gateway that is merely busy. It cannot
            // outlast one that is gone: nothing between cells restarts the process, so the first
            // death forfeits every remaining cell as untestable. plano published one measured cell
            // and two "no connection to the gateway: Connection refused" in both the dd26a54 and
            // 8f2af5d field runs - the same shape twice, the whole grid after the first cell lost to
            // a process that was not there any more.
            //
            // The harness owns this gateway's lifetime (that is what `relaunch` IS - the memory
            // group already stops and starts it every cell), so when the connection is refused after
            // the budget, bring it back and ask once more. Cheap when it works, and when it does not
            // the cell records exactly what it recorded before.
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
                        // A FAILED RESTART POISONS EVERYTHING AFTER IT. The stop may have half
                        // succeeded, the relaunch may be up with its post-boot configuration
                        // half-replayed - a gateway that ANSWERS while misconfigured, which is the
                        // worst state to keep measuring: every later cell's failure would publish
                        // as the gateway's own. The cell keeps its honest verdict; the rest of the
                        // grid is marked untestable naming OUR failure, and the probe loop stops
                        // asking a gateway whose state the harness can no longer vouch for.
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
            // THE ENGINE, IN TWO LINES: if the cell is served, run every metric on it. The list of
            // metrics lives in one place (`metric::METRICS`) rather than being reached for here, so
            // a measurement cannot be implemented, tested, and then silently never taken.
            // RE-VERIFY BEFORE MEASURING, not after. The metrics drive millions of requests through
            // the same recorder the check reads, so a reset afterwards would be racing an eight
            // minute memory window, and the recorder's `body_ok` only ever describes the LAST body it
            // saw. One request, on a cleared recorder, with nothing else in flight.
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
                // WHERE THE CELL'S TIME WENT, on one greppable line. A run that is slower than the
                // last one is otherwise unanswerable from the artifact: the total says a cell took
                // thirteen minutes and nothing says whether that was the TTFT samples, a stream
                // ladder reaching a higher rung, or the gateway itself.
                let mut by_cost: Vec<_> = t.iter().collect();
                by_cost.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));
                let total: f64 = t.values().sum();
                let breakdown = by_cost
                    .iter()
                    .map(|(name, secs)| format!("{name}={secs:.1}s"))
                    .collect::<Vec<_>>()
                    .join(" ");
                // `[cost]`, NOT `[cell]`. The status board parses `[cell N/M] <id>: <verdict>` and
                // counts anything whose verdict is not a cheap outcome as a served cell, so emitting
                // the breakdown under that prefix made every measured cell count twice - "10/8
                // served" and an ETA of zero on a gateway still running. A diagnostic line that
                // corrupts the progress display is worse than no diagnostic line.
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
    #[test]
    fn the_stream_bound_is_physical_and_the_runaway_cap_never_participates() {
        // A CAP CHOSEN NEAR WHERE MEASUREMENTS LIVE BECOMES PART OF THE MEASUREMENT. The first version
        // of this bound was a constant 4096, picked because no gateway had cleanly exceeded c=2178.
        // The field's own sweeps said otherwise - apisix sustained c=16384 with ZERO stalls and every
        // window passing, litellm-rust c=6144, aisix c=4096 - so it would have clipped three gateways
        // and published a smaller rung as their peak. A wrong number is worse than an honest hole.
        //
        // The real stopping condition is measured (see
        // `search::a_ladder_that_ends_on_failing_rungs_publishes_the_best_passing_rung`). The cap here
        // is only a runaway backstop, and these assertions are what keep it from quietly becoming
        // anything more.
        let bench_box_fds = 1_048_576;
        let bench_box_ports = 32_768;
        assert_eq!(
            super::stream_ceiling_from(bench_box_fds, bench_box_ports),
            bench_box_ports,
            "the PHYSICAL bound must decide it - with descriptors raised far above the port range, \
             the port range is the answer and the runaway cap must not be visible in it"
        );
        // WHAT THE BACKSTOP IS FOR, asserted through the function rather than against the constant.
        // This was `STREAM_RUNAWAY_CAP >= 4 * 16_384`, which clippy correctly called a constant
        // assertion: both sides are compile-time literals, so it could never fail at runtime - a dead
        // gate in the very test written to keep a cap honest. Driving the derivation with no physical
        // limit at all shows the backstop is the thing that stops it, and that it sits several
        // doublings above the highest rung the field has cleanly sustained (apisix, c=16384).
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

    // A PACING INTERVAL OF ZERO WOULD FAIL EVERY STREAM ON THE BOARD.
    //
    // `stall_bound_us()` is this interval times STREAM_STALL_MULTIPLIER, so a parsed 0 makes the stall
    // bound 0 and EVERY inter-frame gap counts as a stall - including a gap of nothing. Both stream
    // metrics would then fail on every rung of every cell, and the artifact would read that as thirteen
    // gateways going quiet rather than as one bad environment variable.
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

    // ONE LUCKY WINDOW MUST NOT BECOME A CONFIRMED STREAM CEILING.
    //
    // The confirmation loop opens `held = 1, total = 1` because the bisection's own winning window is
    // a real vote, then takes WINDOWS_PER_RUNG-1 repeats. A repeat that cannot RUN is skipped WITHOUT
    // incrementing `total`, so two absent repeats left 1 of 1 - and the majority test alone reads that
    // as a pass, publishing the single unrepeated window as the ceiling plus its own raw fps as
    // streams_sustained_fps. Both absent paths (RigExhausted on ports/descriptors, a panicked lane)
    // bite hardest at the top rung, which is exactly where the published number comes from.
    //
    // `confirm_ceiling` refuses the same input on the throughput side and explains that one lucky
    // window is how a sustained figure lands ABOVE the peak from the same sweep. The stream copy kept
    // the majority half and dropped the minimum-window half, while its comment claimed to run "the
    // same majority rule" - true only of the half that survived.
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

    // A WINDOW THAT LOST LANES MUST BE DISCARDED, LOUDLY.
    //
    // This rule cost a four-hour run every cpu_fps number it should have produced, and left nothing
    // to diagnose it with. A panicked lane was dropped on the floor - no count, no message - and
    // when every lane went, `streams == 0` returned `None` through the ONE refusal in that function
    // that said nothing. The visible symptom was a metric that took 0.0s and published an absence,
    // on every streamable cell, with no line anywhere in the log or the artifact explaining why.
    //
    // The subtler half is why survivors are not good enough: `streams` and `expected_frames` are
    // both accumulated per surviving lane, so a panicked lane leaves the numerator AND the
    // denominator. A window that lost half its lanes reported the survivors' delivery ratio as the
    // whole window's and passed the gate on it - flattered by the removal of its own failures.
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

    /// THE TWO PUBLISHED THROUGHPUT NUMBERS DESCRIBE ONE SET OF WINDOWS.
    ///
    /// This replays the shape that produced the 2026-07-28 board's three inversions: a cell whose
    /// throughput plateaus early and whose p99 keeps holding well past the plateau. Under the old
    /// engine the peak came from a `saturation_plateau` and the ceiling from a `bisect_ceiling` that
    /// ran three groups later, after `Memory` had cold-restarted the gateway - so the second search
    /// measured a warmer, differently-conditioned process and returned a HIGHER rate than the
    /// "maximum" it was meant to sit under. apisix anthropic>anthropic published 19866 max against
    /// 21153 sustained; kong openai>gemini 22806 against 24035.
    ///
    /// Both numbers now come out of one climb over one process, so the ceiling cannot be reported
    /// above a peak taken from the same rungs.
    #[test]
    fn the_sustained_figure_cannot_be_reported_above_the_peak_it_shares_a_sweep_with() {
        // A gateway that saturates at c=16 and holds its p99 out to c=100 - the aisix shape, where
        // the gate's ceiling sits far above the throughput knee.
        struct Plateau {
            gate_ceiling: u32,
        }
        impl crate::search::Probe for Plateau {
            fn probe(&mut self, c: u32) -> Option<crate::search::Sample> {
                let rps = if c <= 16 {
                    f64::from(c) * 1000.0
                } else {
                    16_000.0
                };
                let p99 = if c <= self.gate_ceiling {
                    5_000
                } else {
                    50_000
                };
                Some(
                    crate::search::Sample::new(rps, true).with_reading(crate::search::Reading {
                        p99_us: Some(p99),
                        ok: 1_000,
                        fail: 0,
                    }),
                )
            }
        }
        let gate = sustained_gate();
        let mut p = Plateau { gate_ceiling: 100 };
        let r = crate::search::saturation_plateau_gated(&mut p, 1, 4096, Some(&gate));
        let peak = r.peak.value().expect("the climb established a peak").value;
        let refined = refine_ceiling(&mut p, &r.rungs);
        let sustained = *refined
            .rps
            .value()
            .expect("the climb established a ceiling");
        let conc = *refined
            .concurrency
            .value()
            .expect("and the concurrency it held at");

        // The climb did not stop at the knee: the gate was still holding there, and stopping would
        // have published c=16 as a ceiling the gateway holds to c=100.
        assert!(
            conc > 16,
            "the ceiling was truncated to the throughput knee: c={conc}"
        );
        assert_eq!(
            conc, 100,
            "the bracket was bisected to the exact boundary, not a ladder rung"
        );
        assert!(
            sustained <= peak,
            "sustained {sustained} exceeded the peak {peak} it was read from - the two numbers are \
             no longer summaries of one sweep"
        );
    }

    /// A gate that NOTHING holds is a measured zero, not an absence: the gateway was asked at every
    /// rung the climb walked and never once kept its p99 under the ceiling.
    #[test]
    fn a_cell_that_never_holds_the_gate_publishes_a_measured_zero() {
        struct NeverHolds;
        impl crate::search::Probe for NeverHolds {
            fn probe(&mut self, c: u32) -> Option<crate::search::Sample> {
                Some(
                    crate::search::Sample::new(f64::from(c) * 10.0, true).with_reading(
                        crate::search::Reading {
                            p99_us: Some(999_999),
                            ok: 100,
                            fail: 0,
                        },
                    ),
                )
            }
        }
        let gate = sustained_gate();
        let mut p = NeverHolds;
        let r = crate::search::saturation_plateau_gated(&mut p, 1, 64, Some(&gate));
        let refined = refine_ceiling(&mut p, &r.rungs);
        assert_eq!(refined.rps.value().copied(), Some(0.0));
        assert_eq!(refined.concurrency.value().copied(), Some(0));
    }

    // THE ERASED-CLIMB DEFECT THIS PINS. When the bisected candidate failed to confirm all the way
    // down (`confirm_ceiling` returned None), `refine_ceiling` published Measured(0.0)/Measured(0) -
    // a zero sitting beside a sweep whose own evidence shows a rung holding the gate by a majority
    // of the climb's windows. The published fallback must be the climb's last proven holder, the
    // same answer the `best == lo` fast path gives, never a fabricated zero.
    #[test]
    fn a_ceiling_that_fails_confirmation_falls_back_to_the_climbs_last_proven_holder() {
        // Holds the gate up to c=21 while the climb and the bisection first look at each rung,
        // then sours: every repeat window at the bisected candidate (c=21) fails, and so does the
        // stepped-down rung (c=10), so `confirm_ceiling` gives up and returns None.
        struct SoursDuringConfirmation {
            calls_at: std::collections::HashMap<u32, u32>,
        }
        impl crate::search::Probe for SoursDuringConfirmation {
            fn probe(&mut self, c: u32) -> Option<crate::search::Sample> {
                let n = self.calls_at.entry(c).or_insert(0);
                *n += 1;
                let holds = c <= 21 && !(c == 21 && *n > 1) && c != 10;
                let p99 = if holds { 5_000 } else { 50_000 };
                Some(
                    crate::search::Sample::new(f64::from(c) * 100.0, true).with_reading(
                        crate::search::Reading {
                            p99_us: Some(p99),
                            ok: 1_000,
                            fail: 0,
                        },
                    ),
                )
            }
        }
        let gate = sustained_gate();
        let mut p = SoursDuringConfirmation {
            calls_at: Default::default(),
        };
        // The climb walks 1,2,4,8,16 (held), 32 and 64 (failed): the last proven holder is c=16.
        let r = crate::search::saturation_plateau_gated(&mut p, 1, 64, Some(&gate));
        assert!(
            r.rungs.iter().any(|s| s.concurrency == 16 && s.gate_holds),
            "the fixture must give the climb a rung it proved: got {:?}",
            r.rungs
                .iter()
                .map(|s| (s.concurrency, s.gate_holds))
                .collect::<Vec<_>>()
        );
        let refined = refine_ceiling(&mut p, &r.rungs);
        assert_eq!(
            refined.concurrency.value().copied(),
            Some(16),
            "a failed confirmation must fall back to the climb's last proven holder, not erase it"
        );
        assert_eq!(
            refined.rps.value().copied(),
            Some(1_600.0),
            "the fallback rate must be that rung's own gate median, never the Measured(0.0) that \
             would put a zero beside evidence of the rung holding"
        );
    }

    /// A CEILING WE NEVER SAW BREAK IS OUR RANGE, NOT THE GATEWAY'S ANSWER.
    ///
    /// `bisect_ceiling` has always refused this - "publishing max_conc would report our own search
    /// bound as the gate's ceiling" - and the sustained refinement published it as Measured. Two
    /// gateways whose real ceilings sit either side of 8000 both come back as exactly `max_conc`,
    /// and the board reads that as agreement between them rather than as a bound we chose.
    #[test]
    fn a_ceiling_still_holding_at_the_top_of_the_range_is_a_lower_bound_not_a_measurement() {
        // Saturates at c=16 and holds its p99 at every concurrency it is ever asked about, so no
        // rung above the top holder ever fails and there is no bracket to bisect.
        struct HoldsEverywhere;
        impl crate::search::Probe for HoldsEverywhere {
            fn probe(&mut self, c: u32) -> Option<crate::search::Sample> {
                let rps = if c <= 16 {
                    f64::from(c) * 1000.0
                } else {
                    16_000.0
                };
                Some(
                    crate::search::Sample::new(rps, true).with_reading(crate::search::Reading {
                        p99_us: Some(5_000),
                        ok: 100_000,
                        fail: 0,
                    }),
                )
            }
        }
        let gate = sustained_gate();
        let mut p = HoldsEverywhere;
        let r = crate::search::saturation_plateau_gated(&mut p, 1, 4096, Some(&gate));
        assert!(
            r.rungs
                .last()
                .is_some_and(|s| s.concurrency == 4096 && s.gate_holds),
            "the fixture must leave the gate holding at the top of the range: got {:?}",
            r.rungs
                .iter()
                .map(|s| (s.concurrency, s.gate_holds))
                .collect::<Vec<_>>()
        );
        let refined = refine_ceiling(&mut p, &r.rungs);
        assert_eq!(
            refined.concurrency.value().copied(),
            None,
            "the top of our own search range must never be published as a measured ceiling"
        );
        assert_eq!(refined.rps.reason(), Some(&Absent::SearchExhausted));
        let detail = refined
            .rps
            .detail()
            .expect("the lower bound travels as prose");
        assert!(
            detail.contains("at least 4096"),
            "the absence must still carry what the search did prove, got {detail}"
        );
        // And the rungs that proved the lower bound still travel as evidence.
        assert!(
            refined
                .points
                .iter()
                .any(|pt| pt.concurrency == 4096 && pt.passed),
            "an absent ceiling still publishes the sweep that explains it"
        );
    }

    /// A FAILED RUNG'S EVIDENCE ROW IS THE EVIDENCE FOR THE CEILING SITTING BELOW IT.
    ///
    /// Every climb rung published `p99_us: None, fail: 0` and a rate taken from `gate_median`, which
    /// is the median of the windows that HELD the gate - so a rung that held in none of them
    /// published `rps: 0` out of that field's `unwrap_or(0.0)`. The verdict flag was right and every
    /// number beside it was invented: a reader re-deriving the ceiling from `sweep_sustained_20ms`
    /// saw a rung that served nothing, when it had served at full rate and been failed for its p99.
    #[test]
    fn a_failed_rungs_evidence_row_carries_what_it_measured_not_placeholders() {
        // Serves 16000 rps at every rung. Above c=64 its p99 blows the 20ms gate and it starts
        // dropping requests - still far under the gate's own 0.1% loss bar, so the p99 is what fails
        // it, and the loss is a real number the row has to carry.
        struct FailsOnLatencyNotOnLoss;
        impl crate::search::Probe for FailsOnLatencyNotOnLoss {
            fn probe(&mut self, c: u32) -> Option<crate::search::Sample> {
                let rps = if c <= 16 {
                    f64::from(c) * 1000.0
                } else {
                    16_000.0
                };
                let over = c > 64;
                Some(
                    crate::search::Sample::new(rps, true).with_reading(crate::search::Reading {
                        p99_us: Some(if over { 30_000 } else { 5_000 }),
                        ok: 100_000,
                        fail: if over { 7 } else { 0 },
                    }),
                )
            }
        }
        let gate = sustained_gate();
        let mut p = FailsOnLatencyNotOnLoss;
        let r = crate::search::saturation_plateau_gated(&mut p, 1, 512, Some(&gate));
        let refined = refine_ceiling(&mut p, &r.rungs);
        let failed = refined
            .points
            .iter()
            .find(|pt| pt.concurrency == 128)
            .expect("the climb probed c=128 and failed it");
        assert!(!failed.passed, "c=128 blew the 20ms gate");
        assert_eq!(
            failed.rps, 16_000.0,
            "the failed rung served 16000 rps at a 30ms p99; publishing 0 says it served nothing"
        );
        assert_eq!(
            failed.p99_us,
            Some(30_000),
            "the p99 that failed the rung is the whole reason it failed, and it was published as null"
        );
        assert_eq!(
            failed.fail,
            Some(7),
            "the loss the window measured, not the 0 the row used to carry"
        );
        // A holding rung keeps publishing what it sustained UNDER the ceiling, with that window's
        // own reading beside it.
        let held = refined
            .points
            .iter()
            .find(|pt| pt.concurrency == 64)
            .expect("the climb probed c=64 and held it");
        assert!(held.passed);
        assert_eq!(held.rps, 16_000.0);
        assert_eq!(held.p99_us, Some(5_000));
        assert_eq!(held.fail, Some(0));
    }

    /// A RUNG WHERE NO WINDOW EVER PASSED PUBLISHES A FABRICATED 0, NOT ITS REAL RATE.
    ///
    /// `search::measure_rung` returns `Rung { median: 0.0, median_reading: None, windows: 0 }` when
    /// every window at a rung comes back with `Sample::passed == false` (`vals.is_empty()`,
    /// search.rs:653-661) - a sentinel, not a measurement. `RungSummary` (search.rs:568-585) copies
    /// `median`/`median_reading` but has no `windows` field, so nothing downstream can tell this rung
    /// apart from one that genuinely served nothing. `refine_ceiling`'s non-holding branch then reads
    /// `r.median` straight into `SustainedPoint.rps`, so this rung publishes `rps: 0.0` in
    /// `sweep_sustained_20ms` even though the window really served 16000 rps and only failed because
    /// every window at that concurrency dropped requests.
    #[test]
    fn a_rung_where_every_window_failed_publishes_a_fabricated_zero_rate() {
        // Serves 16000 rps at every rung, same curve as the sibling test above, but past c=64 it
        // marks the window itself FAILED (the probe's own passed verdict, as `SweepProbe::probe`
        // computes it: `stats.fail == 0 && stats.ok > 0`) rather than merely blowing the 20ms gate.
        struct FailsOutright;
        impl crate::search::Probe for FailsOutright {
            fn probe(&mut self, c: u32) -> Option<crate::search::Sample> {
                let rps = if c <= 16 {
                    f64::from(c) * 1000.0
                } else {
                    16_000.0
                };
                let over = c > 64;
                Some(
                    crate::search::Sample::new(rps, !over).with_reading(crate::search::Reading {
                        p99_us: Some(if over { 30_000 } else { 5_000 }),
                        ok: 100_000,
                        fail: if over { 7 } else { 0 },
                    }),
                )
            }
        }
        let gate = sustained_gate();
        let mut p = FailsOutright;
        let r = crate::search::saturation_plateau_gated(&mut p, 1, 512, Some(&gate));
        let refined = refine_ceiling(&mut p, &r.rungs);
        let failed = refined
            .points
            .iter()
            .find(|pt| pt.concurrency == 128)
            .expect("the climb probed c=128 and failed it");
        assert!(!failed.passed, "c=128 blew the 20ms gate");
        assert_eq!(
            failed.rps, 16_000.0,
            "the rung served 16000 rps at every window; publishing 0 says it served nothing"
        );
    }

    /// A RATE NOBODY RE-MEASURED IS ONE WINDOW'S LUCK, NOT A SUSTAINED CEILING.
    ///
    /// The bracket bisection takes SINGLE windows - correct for a search, cheap and convergent - and
    /// `confirm_ceiling` opens its majority count at 1 with that window as the vote. When both
    /// repeats came back empty, 1 of 1 was a majority and the bisection's own uncorrected `value`
    /// became the cell's published sustained throughput. That is the rate that can land above the
    /// peak measured from the same sweep, which is the inversion C6 flags downstream.
    #[test]
    fn a_candidate_the_rig_could_not_re_measure_is_never_published() {
        // Holds the gate to c=64 and fails above it, so the climb hands over a real 64..128 bracket.
        // Every concurrency the BISECTION invents (never a power of two) answers its first window
        // and then produces nothing at all - the rig running short, not the gateway failing.
        struct RigRunsShortDuringConfirmation {
            calls_at: std::collections::HashMap<u32, u32>,
        }
        impl crate::search::Probe for RigRunsShortDuringConfirmation {
            fn probe(&mut self, c: u32) -> Option<crate::search::Sample> {
                let n = self.calls_at.entry(c).or_insert(0);
                *n += 1;
                if !c.is_power_of_two() && *n > 1 {
                    return None;
                }
                let holds = c <= 64 || !c.is_power_of_two();
                Some(
                    crate::search::Sample::new(f64::from(c.min(64)) * 100.0, true).with_reading(
                        crate::search::Reading {
                            p99_us: Some(if holds { 5_000 } else { 50_000 }),
                            ok: 10_000,
                            fail: 0,
                        },
                    ),
                )
            }
        }
        let gate = sustained_gate();
        let mut p = RigRunsShortDuringConfirmation {
            calls_at: Default::default(),
        };
        let r = crate::search::saturation_plateau_gated(&mut p, 1, 512, Some(&gate));
        let refined = refine_ceiling(&mut p, &r.rungs);
        assert_eq!(
            refined.concurrency.value().copied(),
            Some(64),
            "an unconfirmable bisection candidate must give way to the climb's last proven holder"
        );
        assert_eq!(
            refined.rps.value().copied(),
            Some(6_400.0),
            "and the rate published with it must be that rung's own three-window gate median"
        );
        // The evidence shows exactly why: the candidate is in the sweep with fewer than a rung's
        // worth of windows behind it, so the drop is readable rather than silent.
        let candidate_windows = refined
            .points
            .iter()
            .filter(|pt| !pt.concurrency.is_power_of_two() && pt.concurrency > 64)
            .count();
        assert!(
            candidate_windows > 0,
            "the bisection's own windows must still be published as evidence"
        );
        // The published rung is a CLIMB rung, so the windows behind it are the climb's own
        // `WINDOWS_PER_RUNG`, and its evidence row says it held.
        let windows_at_published = p.calls_at.get(&64).copied().unwrap_or(0) as usize;
        assert!(
            windows_at_published >= crate::search::WINDOWS_PER_RUNG,
            "the published rung must have been measured a full rung's worth, got \
             {windows_at_published}"
        );
        assert!(
            refined
                .points
                .iter()
                .any(|pt| pt.concurrency == 64 && pt.passed),
            "and its evidence row must show it holding the gate"
        );
    }

    /// BOTH GATE SEARCHES CONFIRM THEIR CEILING, or neither claim is worth more than one window.
    ///
    /// The bisection walks up until ONE window fails, so it lands on the boundary by construction.
    /// The sustained ceilings of the 2026-07-28 run held their gate in 1 of 3 windows on 9 of 48
    /// cells - agentgateway c=252 saw 19866, 20036, 20404 against a 20000us gate.
    ///
    /// THIS TEST USED TO ASSERT AGAINST ITS OWN SOURCE FILE. It counted occurrences of the string
    /// `held * 2 > total` in `run.rs` and demanded two, and its own two literals satisfied the
    /// count - so the confirmation could have been deleted from production entirely and it would
    /// still have passed. A test that cannot fail is worse than no test, because it is read as
    /// coverage. This drives the real refinement against a scripted probe instead.
    #[test]
    fn every_published_ceiling_was_measured_by_a_full_rung_of_windows() {
        // The field shape: the boundary rung passes the FIRST time it is asked and fails every
        // re-measurement, which is exactly what a bisection is built to find and exactly what must
        // not be published. Below c=64 the gateway is solid.
        struct PassesOnceAboveTheClimb {
            calls_at: std::collections::HashMap<u32, u32>,
        }
        impl crate::search::Probe for PassesOnceAboveTheClimb {
            fn probe(&mut self, c: u32) -> Option<crate::search::Sample> {
                let n = self.calls_at.entry(c).or_insert(0);
                *n += 1;
                let holds = c <= 64 || (c <= 200 && *n == 1);
                Some(
                    crate::search::Sample::new(f64::from(c.min(64)) * 100.0, true).with_reading(
                        crate::search::Reading {
                            p99_us: Some(if holds { 5_000 } else { 50_000 }),
                            ok: 10_000,
                            fail: 0,
                        },
                    ),
                )
            }
        }
        let gate = sustained_gate();
        let mut p = PassesOnceAboveTheClimb {
            calls_at: Default::default(),
        };
        let r = crate::search::saturation_plateau_gated(&mut p, 1, 512, Some(&gate));
        let refined = refine_ceiling(&mut p, &r.rungs);
        let published = refined
            .concurrency
            .value()
            .copied()
            .expect("the climb proved a ceiling below the boundary");
        assert!(
            published < 127,
            "c=127 passed exactly one window and must not be the published ceiling, got {published}"
        );
        assert_eq!(
            published, 63,
            "the confirmation steps down by halves and publishes the rung that actually held"
        );
        // THE PROPERTY, stated against the probe's own call counts rather than against a string:
        // whatever concurrency is published was asked at least a full rung's worth of times.
        let windows_at_published = p.calls_at.get(&published).copied().unwrap_or(0) as usize;
        assert!(
            windows_at_published >= crate::search::WINDOWS_PER_RUNG,
            "the published ceiling c={published} was measured {windows_at_published} times; a \
             ceiling is confirmed over {} windows or it is not published",
            crate::search::WINDOWS_PER_RUNG
        );
        // And the windows that refused the candidate are in the published sweep, so the step-down is
        // re-derivable rather than asserted.
        assert!(
            refined
                .points
                .iter()
                .any(|pt| pt.concurrency == 127 && !pt.passed),
            "the failing re-measurements at the bisected candidate must travel as evidence"
        );
    }
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    fn cfg_for(gw: SocketAddr, mock: SocketAddr) -> RunConfig {
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

    // THE DEFECT THIS GUARDS AGAINST. `restart_to_rest` used to build a throwaway `RealLauncher` on
    // every call, so the `Child` it spawned was dropped the moment the function returned. The NEXT
    // restart's `pkill` then killed a process nothing could `wait()` on: a zombie, once per served
    // cell, for the whole run. Two restarts against the SAME persistent launcher must not leave the
    // FIRST process a zombie once the second has replaced it.
    #[test]
    fn restart_to_rest_reaps_the_process_it_replaces() {
        // `build_invocation` pins a native gateway with `taskset`, which is Linux-only. The field
        // runs on Linux and so does CI, so this exercises the real launch path there; on a
        // developer's macOS box there is nothing to pin with and the launch would fail for a reason
        // that has nothing to do with reaping. Say so out loud rather than failing on it: a test
        // that reports red for the wrong reason gets ignored, and an ignored test is not a gate.
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

    // THE UNCONFIGURED-RELAUNCH DEFECT THIS GUARDS AGAINST. A gateway with no config file is
    // configured through its own admin API by the manifest's `commands`, run once after the initial
    // launch. For docker a stop is `docker rm -f`, so the database those commands wrote dies with
    // the container - and `restart_to_rest` relaunched WITHOUT replaying them. On the 2026-07-28
    // board one-api lost its three channels at the memory group's restart: throughput (measured
    // before it) published real numbers, and streaming + added latency (measured after it) failed
    // 100%, publishing the missing configuration as the gateway's own failure. The restart must
    // replay the commands, and a command that fails must fail the restart: a half-configured
    // gateway answering probes is worse than a down one.
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

    // THE COLLAPSED EGRESS AXIS THIS PREVENTS. Most gateways choose the upstream from the model name
    // in the request, so a grid that sends one fixed model sends a byte-identical request for all six
    // egress columns of a row. Every column reaches the SAME upstream, and the artifact publishes six
    // translation cells for one measurement the gateway was never asked to perform. The axis reads as
    // measured and is not.
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
                    // Openai-shaped, like `serve_sse` and like the mock: a role head that carries no
                    // token, then content deltas. The delivery gate counts CONTENT frames, so a
                    // ladder of bare `data: f0` events would fail every rung for the wrong reason.
                    let _ = c.write_all(
                        b"data: {\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\"}}]}\n\n",
                    );
                    let frames = if pass(n) {
                        crate::metric::STREAM_FRAME_BUDGET
                    } else {
                        1
                    };
                    for i in 0..frames {
                        let _ = c.write_all(
                            format!(
                                "data: {{\"choices\":[{{\"index\":0,\"delta\":{{\"content\":\"f{i}\"}}}}]}}\n\n"
                            )
                            .as_bytes(),
                        );
                    }
                    // Then close: a passing lane has already hit the client's frame budget, and a
                    // failing lane's early close is what makes the shortfall visible at once.
                });
            }
        });
        addr
    }

    // THE SELF-VOTING STEP-DOWN DEFECT THIS PINS. In `sweep_streams_cell`'s step-down loop the
    // fresh first window at a stepped-down ceiling seeded the next iteration's `held = 1` even when
    // it FAILED the gate, so a rung could reach "majority" with one real hold out of three and fold
    // a gate-failing window's rate into the published sustained figure. A failing stepped window
    // must end the search instead - the same rule `confirm_ceiling` applies on the throughput side.
    //
    // The script, by accept-order connection index (lo=1, hi=4): conns 1-3 are the climb's passing
    // windows at c=1 and c=2; conns 4-15 fail (the climb's c=4, the bisection's c=3, both
    // confirmation windows at c=2, and the fresh window at the stepped-down c=1); conns 16+ would
    // pass. Under the old code the two follow-up windows at c=1 landed on 16-17 and, with the
    // failing fresh window casting the seed vote, c=1 published as a sustained ceiling.
    // A RIG ABORT IS NOT THE GATEWAY'S ZERO, and this pins the key that keeps them apart.
    //
    // `cpu_fps_result_from_search` publishes a MEASURED 0 when the load was offered and every rung
    // failed the gate. An interrupted climb arrives in exactly that shape - rungs present, none of
    // them passing - so the only thing separating "the gateway carried nothing" from "our own rig
    // stopped asking" is the `NotMeasured` reason key. A round-2 audit deleted that key and all 60
    // run:: tests and all 11 search-property tests stayed green, so the rule was real and unguarded.
    #[test]
    fn a_rig_abort_with_every_rung_failing_is_absent_not_the_gateways_measured_zero() {
        let failed_rung = |c: u32| StreamPoint {
            concurrency: c,
            passed: false,
            fps: 0.0,
            frames: 0,
            expected_frames: 64,
            content_frames: 0,
            expected_content_frames: 63,
            streams: c as u64,
            errored: 0,
            stalls: 0,
            why: Some("delivered 0 of 63 expected content frames".to_string()),
        };
        let points = vec![failed_rung(1), failed_rung(2)];

        // The genuine failure: every rung was offered and every rung failed. A real, measured zero.
        let genuine: Measurement<crate::search::PeakPoint> = Measurement::absent_because(
            Absent::NotMeasured,
            "no concurrency from 1 to 2 passed the gate",
        );
        let r = cpu_fps_result_from_search(&genuine, points.clone());
        assert_eq!(
            r.fps.copied(),
            Some(0.0),
            "the gateway carried no clean frames, which is a 0"
        );
        assert_eq!(r.concurrency.copied(), Some(0));

        // The rig abort, in the SAME shape: rungs present, all failing. It must stay absent.
        let interrupted: Measurement<crate::search::PeakPoint> = Measurement::absent_because(
            Absent::RigLimited,
            "probe interrupted; the curve was never observed to turn over",
        );
        let r = cpu_fps_result_from_search(&interrupted, points);
        assert_eq!(
            r.fps.copied(),
            None,
            "an interruption is the rig failing to finish asking, never the gateway's measured zero"
        );
        assert_eq!(r.fps.reason(), Some(&Absent::RigLimited));
        assert!(r.fps.detail().unwrap_or_default().contains("interrupted"));
        assert_eq!(r.concurrency.reason(), Some(&Absent::RigLimited));
    }

    #[test]
    fn a_stepped_down_stream_rung_whose_fresh_window_fails_ends_the_search_without_a_vote() {
        let gw = sse_ladder_server(|n| n <= 3 || n >= 16);
        let cfg = cfg_for(gw, gw);
        let id = CellId::new("openai", "openai");
        let r = sweep_streams_cell(&cfg, &id, 1, 4);
        assert_ne!(
            r.concurrency.value().copied(),
            Some(1),
            "the stepped-down rung must not publish on the strength of its own failing seed window"
        );
        assert_eq!(
            r.concurrency.value().copied(),
            None,
            "no rung held a majority of real windows, so nothing was proven sustained"
        );
        assert!(
            r.fps
                .detail()
                .unwrap_or_default()
                .contains("did not hold the stream gate"),
            "the absence must carry why the confirmed ceiling was not published: {:?}",
            r.fps.detail()
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
        // An explicit, empty metric list: this test is about the SHAPE of the grid, so it must not
        // pay for every real measurement to assert that every pairing appears.
        let rows = run_grid_with(&cfg, 1, 2, &[]);
        assert_eq!(rows.len(), 4);
    }

    /// A relaunch spec that cannot come back up: its stop path matches no process (so stopping
    /// "succeeds" instantly) and its binary does not exist, so `restart_to_rest` fails fast on any
    /// platform - no taskset gating needed, because the FAILURE path is what is under test.
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

    // THE UNVOUCHED-PROCESS DEFECT THIS PINS. A mid-grid restart that FAILED used to be one eprintln
    // and business as usual: the loop went on probing a gateway whose stop may have half succeeded
    // and whose relaunch may be up half-configured, so every later cell's failure published as the
    // gateway's own. After a failed restart, every remaining cell must be recorded untestable with a
    // detail naming the HARNESS's failure, and nothing after it may be probed or measured.
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

    // A cell the manifest declares OUT of its capability grid must never be probed at all, even when
    // the server sitting behind it would happily answer 200: the declaration wins, unconditionally,
    // because probing it anyway and grading whatever came back is exactly the defect this field
    // exists to prevent (a global gate answering for a pairing that was never really asked).
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

    // EVERY FRAME, and the rung below is where the gateway's real ceiling is. A dropped frame is a
    // dropped token; there is no concurrency at which losing one is the gateway succeeding.
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

    // Both stream searches judge a window the same way. They did not: the cpu-fps probe passed on
    // `errored == 0 && frames > 0`, so a window delivering 1 frame of 64 counted as a healthy rung
    // and its frames/sec was published.
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

    // A window that opened nothing measured nothing. A ratio computed from zero must never read as a
    // clean window by accident of floating-point division - the same trap `sustained_gate_passes`
    // guards for its own all-zero window.
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

    // A window with no measurable duration measured no RATE. An infinity here would win every peak
    // search it appeared in and publish a rig artefact as the gateway's frames/sec ceiling.
    #[test]
    fn a_window_with_no_elapsed_time_reports_no_rate_rather_than_an_infinity() {
        let mut w = clean_stream_window(4, 4);
        w.elapsed_s = 0.0;
        assert_eq!(w.fps(), 0.0);
        assert!(w.fps().is_finite());
    }

    // ── the stall bound itself ───────────────────────────────────────────────────────────────────

    // Gaps, not the first frame's offset: a gateway that is merely slow to start the stream has not
    // stalled, and charging its time-to-first-token here would fail it on the number the `Streaming`
    // group already publishes as a difference.
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

    /// A minimal SSE peer, SHAPED LIKE THE MOCK'S OPENAI STREAM: one role head frame carrying no
    /// content, then `frames` content deltas `gap_ms` apart, then the connection closes. Enough to
    /// drive `stream_window` for real - the gate above is pure, but nothing else proves the lanes
    /// actually open, read frames, and are joined.
    ///
    /// The head frame is not decoration. A peer that sent nothing but content deltas could not
    /// distinguish the delivery ratio RIG-11 fixed from the one it replaced, because the two agree
    /// exactly when no scaffolding arrives.
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
        // NOT `stalls == 0`. A stall is a frame gap wider than twice the mock's pace, so on a
        // machine running the rest of this suite in parallel it is a fact about the machine: this
        // asserted zero and observed 2 whenever a neighbouring window test ran beside it. What the
        // window is actually being held to is that stalls do not stop a clean full-delivery window
        // from holding the gate, which the gate assertion below says directly and without depending
        // on how busy the box is.
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

    /// A peer that answers a well-formed JSON document rather than an event stream. Declares its
    /// content-type, which is what lets `post_json_sse` answer immediately instead of waiting out its
    /// deadline - the same short-circuit a real non-streaming gateway gets.
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

    // A peer that answers plain JSON has not streamed. That is an ERRORED stream, not a delivery
    // shortfall: the two halves of the README's gate are different findings, and folding a
    // non-streaming peer into "delivered fewer frames" would hide it behind a ratio.
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

    // A stream that opens and then delivers SHORT is not an error, it is a delivery shortfall. The
    // gate refuses it on the delivery bar, and the error count stays clean, so the published rung
    // says which of the two actually happened.
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

    // THE CLOCK STARTS AFTER THE LANES EXIST, and that is asserted at the clock site rather than
    // through the stopwatch, because the stopwatch cannot see it.
    //
    // This test used to assert `elapsed_s < 0.25`. That constant is the speed of the machine that
    // wrote it: green on CI, stably red at 0.355s on a developer's Mac, and red on any loaded
    // runner. It was never measuring the ordering it is named for.
    //
    // Measured, rather than assumed: spawning the 200 lanes costs 2.4ms of a 380ms window - 0.6%.
    // Starting the clock before the spawn loop instead of after it therefore moves `elapsed_s` by
    // less than the run-to-run scatter, and an injected version of exactly that defect passes every
    // timing bound loose enough not to fail on a slow machine. Lane setup stopped being expensive
    // when it stopped being serial OS-thread creation, which is the same change that made the
    // ordering matter less; the bound outlived the ramp it was written for.
    //
    // So this keeps what a window CAN be held to - every lane ran, every frame was counted, and the
    // clock is positive and finite - and leaves the ordering to the comment at `started`, which is
    // where a reader changing it will actually be standing.
    #[test]
    fn a_stream_window_counts_every_lane_it_was_asked_for() {
        // A fleet, but not a stampede. 200 lanes here starved a neighbouring window test into
        // reporting stalls under parallel load, and since the ramp assertion is gone the extra
        // lanes bought nothing but contention - a test that fails its neighbours is a bad test.
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
        // `sustained_gate_passes` is only ever called after `SustainedProbe::probe` has already
        // filtered out the all-zero window as unmeasured (see its own `stats.ok == 0 && stats.fail
        // == 0` guard). This pins the function's own behaviour on that input regardless: a fail
        // ratio computed as 0/0 must never read as a CLEAN window by accident of floating-point
        // division.
        assert!(!sustained_gate_passes(Some(1), 0, 0));
    }

    // ── the two legs must sample the same way, or C6 compares two different questions ────────────
    //
    // The maximum is a MEDIAN of three windows at its rung. The sustained ceiling used to publish a
    // SINGLE window at its own. On a gateway whose windows scatter, one sample can land on the fast
    // mode where a median of three cannot, so the sustained leg beat the "maximum" on the same box
    // against the same mock and C6 correctly refused the board.
    //
    // The 2026-07-28 field run makes the pattern unambiguous: all six inverted cells had a sweep
    // scatter between 65% and 105%, and not one cell with a steady sweep inverted.
    #[test]
    fn a_scattering_ceiling_publishes_its_median_not_whichever_window_came_first() {
        // kong openai>openai's own windows at one concurrency, in the order the rig took them.
        // Taken first: 18672. The fast mode 23761 is real but it is not the typical rate, and
        // publishing it is what let "sustained" exceed a "maximum" measured as a median.
        let kong = [(23761.0, true), (18824.0, true)];
        assert_eq!(
            sustained_median(18672.0, &kong),
            18824.0,
            "the ceiling must publish the middle window, not the fast mode"
        );
        // The defect being fixed, stated as its own assertion: the first window alone is NOT the
        // answer once repeats exist.
        assert_ne!(
            sustained_median(23761.0, &[(18672.0, true), (18824.0, true)]),
            23761.0
        );
        // ...and the median does not depend on which window happened to be taken first.
        assert_eq!(
            sustained_median(23761.0, &[(18672.0, true), (18824.0, true)]),
            sustained_median(18672.0, &[(23761.0, true), (18824.0, true)])
        );
    }

    // A window that blew the p99 ceiling sustained nothing. Folding its rate into a SUSTAINED number
    // would publish throughput the gate had just refused, which is why `search::measure_rung`
    // excludes failed windows from the peak's median too.
    #[test]
    fn a_window_that_failed_the_gate_is_not_folded_into_the_sustained_number() {
        // The failing window is the FASTEST - blowing the latency ceiling usually means more
        // requests, not fewer - so counting it would drag the published number UP, not down. Stated
        // over three held windows so the assertion is about the FILTER rather than about which of
        // two the median lands on: nearest-rank over an even count takes the lower, which is the
        // engine's one convention (`stats::nearest_rank_index`) and not this rule's business.
        let with_failure = [(90000.0, false), (20000.0, true), (21000.0, true)];
        assert_eq!(sustained_median(19000.0, &with_failure), 20000.0);
        assert!(
            sustained_median(19000.0, &[(90000.0, false), (20000.0, true)]) < 90000.0,
            "the failing window must not reach the median at any sample count"
        );

        // Nothing survives the filter: fall back to the bisection's own window, which is a real
        // measured reading that already proved this ceiling - never a zero, and never the failure.
        assert_eq!(
            sustained_median(19000.0, &[(90000.0, false), (91000.0, false)]),
            19000.0
        );
        // The rig returning nothing at all is the same case: one fewer sample, not a zero.
        assert_eq!(sustained_median(19000.0, &[]), 19000.0);
    }

    // A SERVER THAT IS BUSY NOW AND FINE IN A MOMENT MUST NOT BE RECORDED AS INCAPABLE.
    //
    // This is the defect that cost a board. `transient_budget()` - 3 attempts, 30s apart - existed,
    // was documented as the budget `Verdict::Failed` had spent, was unit-tested, and was called by
    // nothing. So one 503 became "this gateway does not serve this pairing", permanently, in public.
    //
    // The harness provokes exactly this: cells run back to back with no settle and the metric before
    // each probe is a heavy load, so a gateway with admission control is still shedding when the next
    // cell asks whether it exists. In the 2026-07-28 field run busbar answered 503 on 26 of 36 cells
    // and every one published as a red, while every egress lane answered fine under openai ingress
    // in the same run - the lanes were healthy, the moment was not.
    //
    // The pause is shortened here through the same budget the field uses, so the test exercises the
    // real loop rather than a copy of it.
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

    // A GATEWAY THAT DOES NOT ANSWER IN TIME IS A MOMENT, NOT A CAPABILITY.
    //
    // The status door and the transport door lead to the same place. busbar lost 26 cells to a
    // transient 503; litellm-python lost three to a timeout, its served count sliding 8 -> 7 -> 5
    // across the 2026-07-28 runs while the lost cells recorded "the gateway accepted the connection
    // and never answered". Both are the harness asking a question of a gateway still shedding the
    // load the harness itself just applied.
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

    // THE STALL BOUND FOLLOWS THE MOCK'S ACTUAL PACE, not a copy of its default.
    //
    // A stall is a frame gap wider than twice the upstream's delta interval, so the bound is only
    // meaningful if it tracks the interval the mock is really using. It was a 20 hardcoded in the
    // engine to match MOCK_STREAM_INTERVAL_MS's default, described as a documented coupling - which
    // is the same two-places-one-truth shape as the two hand-rolled ladders, and right up until
    // someone uses the knob. Turn the mock's pacing down and every streaming rung is judged against
    // a cadence nothing is producing, with no error anywhere.
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

    // THE CEILING IS THE HOST'S, AND EVERY NUMBER IN IT IS READ OR DERIVED.
    //
    // 4096 was picked when the generator was thread-per-connection, and replacing it with a bigger
    // constant only moved the arbitrary number: raising it to 65536 asked for more connections than
    // a single host can make to a single destination, because a TCP connection needs a unique
    // 4-tuple and the source ports run out first. Stock Linux allows about 28,000 of them.
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

    // THE RIG RUNNING OUT IS NOT AN ERRORED STREAM.
    //
    // The load generator got this treatment first; the stream path is where it bites soonest,
    // because the stream searches reach high concurrency before anything else does. A lane that
    // could not get an ephemeral source port never asked the gateway anything, and counting it as an
    // errored stream published our own exhaustion as the gateway's concurrent-stream ceiling.
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

    // A CEILING THE GATEWAY HOLDS ONE TIME IN THREE IS NOT A CEILING IT SUSTAINS.
    //
    // The bisection walks up until ONE window fails, so it lands exactly on the boundary: the
    // highest concurrency that passed once. Re-measuring the 2026-07-28 field run's own published
    // ceilings found 9 of 48 held the p99 gate in only 1 of 3 windows, and the shape is unmistakable
    // - the first window passes and the rest fail. agentgateway c=252 saw 19866, 20036, 20404 against
    // a 20000us gate; apisix c=171 saw 19980, 21114, 22530.
    #[test]
    fn a_ceiling_is_confirmed_by_a_majority_of_its_own_windows() {
        // The rule the confirmation applies, with the bisection's own winning window counted as one
        // vote - it is a real measurement at that concurrency and discarding it throws away evidence.
        let holds = |repeats: &[bool]| {
            let held = 1 + repeats.iter().filter(|ok| **ok).count();
            let total = 1 + repeats.len();
            held * 2 > total
        };

        // The field shape: bisection window passed, both confirmations failed. 1 of 3 is not a
        // ceiling, and this is the case that was being published.
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

    // A PROBE WE REFUSE TO SEND IS UNTESTABLE, NEVER A CAPABILITY VERDICT.
    //
    // Ledger RIG-12: the probe lane composed the same manifest headers the load lane did and
    // interpolated them raw. Now `http::send` refuses, and this is what the refusal must become on
    // the way out - `Served::Untestable`, the spelling of `probe::Verdict::NotVerified`, which is
    // "a statement about the RIG, not the gateway". `Served::No` here would convict a gateway of a
    // defect in one of our own manifest files.
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

    // A STREAM WINDOW WE REFUSE TO SEND MEASURED NOTHING, so it is unmeasured - not a failing rung,
    // and not an errored stream. Charging the gateway for a header of ours is the attribution
    // inversion this engine refuses everywhere.
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

    /// A peer that answers with a full budget of well-formed openai events, NONE of which carries a
    /// token: the role head, then finish/`[DONE]` scaffolding repeated. A real gateway does not do
    /// this; the point is that nothing in the old accounting could tell it apart from one that
    /// streamed perfectly.
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

    // A STREAM THAT DELIVERED NO TOKENS HAS NOT DELIVERED.
    //
    // Ledger RIG-11. `delivery_ratio` was `frames / expected_frames` over every dispatched SSE
    // event, so a peer that filled its whole frame budget with framing satisfied the gate - ratio
    // 1.0, no stalls, no errors, published as the gateway carrying clean streams at that
    // concurrency. The counts a reader needs to see this are on the window either way; only the
    // ratio's numerator changed.
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

    /// An SSE peer SHAPED LIKE A GATEWAY RATHER THAN LIKE THE MOCK: openai's role head, then a
    /// SECOND framing event the mock never sends, then `content` tokens with a keepalive between
    /// each pair. Every token the client asks for is delivered; the stream just costs more events
    /// than the mock's own layout to deliver them.
    ///
    /// This is not a hypothetical peer. Anthropic's real SSE protocol sends `ping` events, a
    /// TRANSLATION cell has the gateway re-emitting the stream in the client's dialect with its own
    /// framing, and a keepalive does the same thing on any dialect.
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
                    // Two prelude events where the mock sends one: the gateway's framing is its own.
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
    // The delivery denominator is `STREAM_FRAME_BUDGET - stream_prelude_frames()`, a constant read
    // off the MOCK's layout, while the numerator is measured on the GATEWAY's stream. While the read
    // stopped after a fixed number of EVENTS, every ping or extra prelude chunk the gateway emitted
    // displaced exactly one content frame: this peer delivers every token asked of it, and an
    // event-budgeted read would have collected 31 of the 63 expected content frames - a ratio of
    // 0.49 against a bound of 1.0, failing AT EVERY RUNG, for a shortfall the gateway did not cause.
    // Reading to the CONTENT budget instead asks the question the metric is for.
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

    /// A peer that frames FOREVER and never sends a token: the pathological case a content-budgeted
    /// read has to be bounded against, since the tokens it is waiting for are never coming.
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

    // THE READ STAYS BOUNDED WITHOUT WAITING OUT THE TIMEOUT, and the shortfall is still a failure.
    //
    // A budget counted in content frames has no bound of its own against a peer that keeps framing,
    // and `STREAM_TIMEOUT` alone is 20 seconds per lane - at the concurrencies these searches climb
    // to that is a search that never returns. `STREAM_EVENT_CEILING` stops the read at 4x the frame
    // budget; hitting it with no tokens is a real delivery shortfall, and the gate says so.
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

    // TWO `authorization` HEADERS ON ONE REQUEST IS A MEASUREMENT NOBODY CAN ATTRIBUTE.
    //
    // `headers_for` composed the dialect's credential header and then appended the manifest's
    // verbatim, and `gateways/litellm-rust/definition.json` declares `Authorization: Bearer
    // {GW_AUTH}` today. HTTP does not define which of two same-named headers a server honours, so
    // the gateway authenticated as somebody and published a clean number for a request whose tenant
    // and route we could not state.
    #[test]
    fn only_one_copy_of_a_header_the_rig_owns_reaches_the_wire() {
        let a: SocketAddr = "127.0.0.1:1".parse().expect("addr");
        let mut cfg = test_fixture(a, a);
        // Spelled as the live manifest spells it, capital A and all: HTTP header names are
        // case-insensitive, and a rule that only caught the lowercase one would not have caught the
        // manifest that motivated it.
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
        // Nothing else is dropped: a routing header is how a column selects its upstream, and losing
        // one would publish a number for a pairing that was never driven.
        assert!(
            h.contains(&("x-route".to_string(), "keep-me".to_string())),
            "{h:?}"
        );

        // The dialect decides which names it owns, so anthropic's protocol constant is protected
        // too - two `anthropic-version` headers is the same undefined resolution.
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

    // A FABRICATED ZERO IN PUBLISHED EVIDENCE.
    //
    // `SustainedPoint.fail` was an `i64` whose only value for "no window reported" was 0, so a climb
    // rung whose windows all came back without a reading published `fail: 0` in
    // `sweep_sustained_20ms` - a row saying the gateway lost nothing at a rate it was never observed
    // serving. The board's rule is that an absent measurement publishes null with a reason; the type
    // had no way to obey it.
    #[test]
    fn a_rung_no_window_reported_publishes_no_failure_count_rather_than_zero() {
        // A probe that answers with a value and a verdict but never a reading - the shape a window
        // that ran out of time or produced nothing leaves behind.
        struct Readingless;
        impl crate::search::Probe for Readingless {
            fn probe(&mut self, _c: u32) -> Option<crate::search::Sample> {
                Some(crate::search::Sample::new(1000.0, true))
            }
        }
        let rungs = vec![
            crate::search::RungSummary {
                concurrency: 8,
                median: 1000.0,
                gate_holds: true,
                gate_median: Some(1000.0),
                median_reading: None,
                gate_median_reading: None,
                windows: 3,
                observed_median: Some(1000.0),
                observed_median_reading: None,
            },
            crate::search::RungSummary {
                concurrency: 16,
                median: 1000.0,
                gate_holds: false,
                gate_median: None,
                median_reading: None,
                gate_median_reading: None,
                windows: 3,
                observed_median: Some(1000.0),
                observed_median_reading: None,
            },
        ];
        let refined = refine_ceiling(&mut Readingless, &rungs);
        for pt in refined.points.iter().filter(|pt| pt.concurrency <= 16) {
            assert_eq!(
                pt.fail, None,
                "no window at c={} carried a reading, so there is no loss count",
                pt.concurrency
            );
        }
    }
}
