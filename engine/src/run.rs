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

/// Where to send this dialect's probe: the gateway's declared path when it is a longer form of this
/// dialect's standard one, otherwise the standard.
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
/// Two axes, and they are genuinely different things. The auth header belongs to the protocol the
/// client is speaking and is identical across gateways, so it comes from `Dialect`. The routing
/// header belongs to the gateway and is how some of them decide which upstream to call, so it comes
/// from the manifest, keyed by column. Collapsing them into one hardcoded shape is what sent
/// `authorization: Bearer` to dialects that do not use one.
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
///
/// THE SAME RULE BETWEEN THE MANIFEST'S OWN TWO SOURCES. A name already sent - by the dialect, by
/// an always-on header, or by an earlier egress header - is not sent again, so the precedence is the
/// order above (dialect, then always-on, then egress) and the wire never carries two copies of one
/// name whichever two sources declared it.
pub(crate) fn headers_for(
    cfg: &RunConfig,
    ingress: Dialect,
    egress: &str,
) -> Vec<(String, String)> {
    let mut out = ingress.auth_headers(&cfg.auth);
    // Case-insensitively, because HTTP header names are (`HttpResponse::header` already reasons this
    // way) and `Authorization` vs `authorization` is the same header to every server that reads it.
    let push = |out: &mut Vec<(String, String)>, (n, v): (String, String)| {
        if out.iter().any(|(have, _)| have.eq_ignore_ascii_case(&n)) {
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
    ceiling_for_port_range(lo, hi)
}

/// The derivation `host_connection_ceiling` applies to a port range, PURE so it can be asserted
/// against ranges other than the one the test host happens to have: the largest power of two that
/// fits `lo..=hi`. The caller guarantees `hi > lo`.
fn ceiling_for_port_range(lo: u32, hi: u32) -> u32 {
    let usable = hi.saturating_sub(lo).saturating_add(1);
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
/// wide margin on a STOCK box.
///
/// ON THE BENCH BOX IT IS NOT, AND THIS GUARD NEVER BINDS THERE. `run-on-ec2.sh` raises the fd limit
/// to 1,048,576, so the descriptor term is 1048576/3 rounded down to a power of two = 262,144, while
/// the port term is 32,768. `min()` therefore picks ports on every field run and the descriptor half
/// of this derivation has never once participated in a result. The 2026-07-29 run climbing apisix to
/// c=32,768 held-open streams is what that looks like: the guard written to stop it was inert.
///
/// NOT PAPERED OVER WITH A SMALLER NUMBER, deliberately. The note above on `STREAM_RUNAWAY_CAP`
/// explains why - litellm-rust reached c=6,144 and aisix c=4,096 with every window passing, so a cap
/// chosen anywhere near where measurements live would have clipped three gateways and published a
/// smaller rung as their peak, which is worse than an honest hole. A ceiling chosen near where
/// measurements live becomes part of the measurement.
///
/// What actually binds at 32,768 concurrent lanes is neither ports nor descriptors: it is MEMORY AND
/// CPU IN THE ENGINE PROCESS, which holds an `SseReader` with its buffers per lane on a box that is
/// also running the pinned gateway and the mock. That is the rig saturating, and the ladder cannot
/// tell it apart from a gateway that is still fast - so the honest fix is to DETECT and disclose rig
/// saturation, not to guess a number that stops the climb before it. Left as a known gap rather than
/// closed with an invented threshold.
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

/// How a probe that reached no gateway at all says so. ONE constant because `run_grid_streaming`
/// keys its mid-grid restart on this text: rewording the probe's message without it would silently
/// disable the restart and forfeit the rest of the grid after the first gateway death.
const NO_CONNECTION_PREFIX: &str = "no connection to the gateway";

/// Ask the gateway whether it serves this pairing. The answer comes only from what was OBSERVED:
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
        Outcome::Response(r) if (200..300).contains(&r.status) => {
            if ing.valid_nonstream_response(r.body()) {
                (Served::Yes, None)
            } else {
                (
                    Served::No(
                        Verdict::Failed,
                        crate::cell::Evidence {
                            status: r.status,
                            body_snippet: crate::cell::Evidence::snippet(&format!(
                                "invalid {ing} response body: {}",
                                String::from_utf8_lossy(r.body())
                            )),
                        },
                    ),
                    None,
                )
            }
        }
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
            Served::Untestable(format!("{NO_CONNECTION_PREFIX}: {e}")),
            Some("the connection was refused".to_string()),
        ),
        // THIS HOST ran out of ports or descriptors, so the gateway was never asked. Untestable like
        // a refused connection, but NOT worded with `NO_CONNECTION_PREFIX`: that text is what makes
        // the grid restart the gateway, and restarting it cannot give this host a free port. Worth
        // another ask, because ports drain out of TIME_WAIT while the probe pauses.
        Outcome::RigExhausted(e) => (
            Served::Untestable(format!(
                "this host could not open a connection (ephemeral ports or descriptors exhausted), \
                 so the gateway was never asked: {e}"
            )),
            Some("this host had no free connection to make".to_string()),
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

/// Drive one pinned load window against the gateway and read the generator's stats line back.
///
/// Shared by the throughput search and the memory window so both put load on the box the same way:
/// same binary, same pinning, its own process. A memory number taken under a differently-generated
/// load is not comparable with a throughput number taken under this one.
pub fn load_window(
    cfg: &RunConfig,
    path: &str,
    body: &str,
    headers: &[(String, String)],
    concurrency: u32,
) -> Option<GenStats> {
    load_window_at(cfg, cfg.gateway_addr, path, body, headers, concurrency)
}

/// The same window, plus WHAT THE GATEWAY SPENT SERVING IT.
///
/// The counters are read from the gateway's process tree immediately before and after the window, so
/// the difference covers this window and nothing else. That ordering is the whole design: an absolute
/// `utime` carries the process's startup, its config parse and every earlier window, and charging
/// those to this window's requests would make CELL ORDER look like a gateway property - the first
/// cell measured would always look the most expensive.
///
/// The cost is `Absent`, never zero, whenever it cannot be taken: the pid may not resolve (the
/// gateway is not up, or the runtime hides it), `/proc` may not exist (a non-Linux host), or a
/// counter may go backwards (pid reuse - see `procsample::cost`). A gateway that we failed to
/// measure must never read as a gateway that used no CPU.
///
/// SAMPLING IS NOT FREE AND IS DELIBERATELY OUTSIDE THE WINDOW. Both reads walk `/proc` once, which
/// is the same scan the RSS sampler already performs on a timer; doing them before and after rather
/// than during means the observation cost cannot land inside the interval being measured.
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
    // The pinned-core utilisation is read from the SAME declaration taskset was given. An empty
    // spec (the `smoke` path, which drives a gateway this process never pinned) yields no cores and
    // therefore an absent utilisation - measuring some other process's cores and labelling the
    // result this gateway's would be worse than reporting nothing.
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
    // THE CLOCK SPANS EXACTLY WHAT THE CPU DIFFERENCE SPANS: from the `before` read to the `after`
    // read. It used to start ahead of the core parse, the /proc/stat scan and the first process-tree
    // walk, and stop after `cost()` and a second /proc/stat scan - wall time with no gateway CPU
    // counted against it, which biased utilisation low by an amount that grew with the size of the
    // gateway's process tree and so differed by gateway.
    let started = std::time::Instant::now();
    let stats = load_window_at(cfg, cfg.gateway_addr, path, body, headers, concurrency);
    // Re-resolve rather than reuse `pid`: a gateway is free to restart between the two reads, and a
    // second sample taken from a DIFFERENT process would be subtracted from the first as though it
    // were the same one. Re-resolving means a restart shows up as a backwards counter, which
    // `procsample::cost` already refuses as a harness error rather than publishing a negative.
    let after = match crate::rss::root_pid(&cfg.runtime).copied() {
        Some(p) => crate::procsample::sample_live(p),
        None => crate::measurement::Measurement::absent_because(
            crate::measurement::Absent::NotMeasured,
            "the gateway's root pid did not resolve after the window",
        ),
    };
    let elapsed_s = started.elapsed().as_secs_f64();
    // Requests come from the window's OWN completed count, never from its published rate: the rate is
    // already a derived figure, and deriving cost from a derivation makes one number's error the
    // other's too.
    let requests = stats.as_ref().map(|s| s.ok).unwrap_or(0);
    let cost = crate::procsample::cost(&before, &after, requests, cfg.sweep_duration_s as f64);
    let cpu_after = if cores.is_empty() {
        None
    } else {
        crate::procsample::cpu_busy_total(&crate::rss::RealProc, &cores)
    };
    // UTILISATION IS DERIVED FROM THE GATEWAY OWN CPU, NOT FROM /proc/stat. It was the other way
    // round for exactly one field run, and tensorzero proved it wrong.
    //
    // /proc/stat per-CPU counters are TICK-SAMPLED; per-process utime/stime is accounted by the
    // scheduler at every context switch. Those disagree badly for a bursty workload. Measured on a
    // live box: tensorzero accumulated 66-255 jiffies in five seconds while its four pinned cores
    // reported 3-18 busy - a 14x to 41x undercount - because it serves ~380us requests that begin and
    // end between ticks. The tell was the denominator: total jiffies for those cores read 1741-1933
    // per five seconds instead of ~2000, and FELL as load rose.
    //
    // A continuously-busy gateway does not show this, which is why eleven of twelve passed the
    // cross-check and only the bursty one failed. The error is therefore not uniform and cannot be
    // corrected for - it is worst exactly where "is this gateway CPU-bound?" is most interesting.
    //
    // Wall time is MEASURED (above, around the two samples) rather than taken from
    // `sweep_duration_s`, which is the CONFIGURED length and not what elapsed.
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
    // The tick-sampled reading is computed but NOT published as utilisation. Kept so the two can be
    // compared deliberately: where a gateway is continuously busy they agree, and a large gap is
    // itself the signal that the workload is bursty.
    let _tick_sampled = crate::procsample::utilisation(cpu_before, cpu_after);
    (stats, cost, util)
}

#[cfg(not(test))]
fn loadgen_executable() -> std::io::Result<std::path::PathBuf> {
    std::env::current_exe()
}

#[cfg(test)]
fn loadgen_executable() -> std::io::Result<std::path::PathBuf> {
    // A lib-test executable accepts test-name filters, not the loadgen command.
    // CI builds the real CLI first; native qualification may name its release binary.
    let exe = if let Some(path) = std::env::var_os("OTB_TEST_ENGINE_BIN") {
        std::path::PathBuf::from(path)
    } else {
        let test = std::env::current_exe()?;
        test.parent()
            .and_then(std::path::Path::parent)
            .ok_or_else(|| std::io::Error::other("cannot locate the CLI beside the test target"))?
            .join("otb")
    };
    if !exe.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "build the otb CLI before library load-window tests (cargo build --bin otb --locked)",
        ));
    }
    Ok(exe)
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
        // Same reasoning as the spawn failure below: a rig that cannot find its own binary empties
        // every window of the run, so it must not do so in silence.
        let exe = match loadgen_executable() {
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
            // The credential rides in the ENVIRONMENT, not the argument list: a token on a command
            // line is visible in `ps` to every user on the box for the life of the window.
            .env(
                crate::loadgen::HEADERS_ENV,
                crate::loadgen::encode_headers(headers),
            )
            .stderr(std::process::Stdio::inherit())
            .output();
        // THE ONE FAILURE THAT KILLS EVERY WINDOW IN THE RUN WAS THE SILENT ONE.
        //
        // This was `.output().ok()?`, which discards the spawn/IO error entirely. If `taskset` is not
        // on PATH - a minimal container, any non-util-linux box - or this binary cannot be re-executed,
        // the child never runs and `None` travels up through every rung of every throughput and
        // sustained search for every cell. The artifact then reads
        // "no load window completed at c=X" and blames the search or the gateway for a missing binary
        // on the rig. Every neighbouring path already reports its cause (spawn_failed, rig_refused, the
        // HarnessError below, gen.rs's runtime-build failure); this one, uniquely, did not - and it is
        // the only one whose blast radius is the whole run rather than one window.
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
        parsed.into_value().map(gen_stats_from_child)
    }
}

/// The child's stats line, as the `GenStats` every caller reads.
///
/// Separate from `load_window_at` so the conversion - the only place the subprocess path decides
/// what a percentile is - can be tested without spawning a child.
fn gen_stats_from_child(u: crate::loadgen::UgenStats) -> GenStats {
    // A WINDOW THAT COMPLETED NOTHING HAS NO PERCENTILE. The child computes p50/p99 over its
    // successes only and prints 0 for an empty set, so passing that through published a collapsed
    // rung (every request failed) as `p99_us: 0` - an absence dressed as the best tail on the board.
    let percentile = |v: i64| (u.ok > 0).then_some(v.max(0) as u64);
    GenStats {
        ok: u.ok.max(0) as u64,
        fail: u.fail.max(0) as u64,
        // `u.rps` is f64 now (fractional below 1/s), so the comparison and the division are
        // both float. A sub-1/s window previously could not even reach here: it failed the i64
        // parse and the whole window was classified as a HarnessError.
        elapsed_s: if u.rps > 0.0 {
            u.ok as f64 / u.rps
        } else {
            0.0
        },
        latencies_us: Vec::new(),
        // FROM THE CHILD, not assumed. This was hardcoded `false`, so `if stats.spawn_failed` in
        // `SweepProbe::probe` - the check that stops the search when the OS refused a thread -
        // could never fire on the subprocess path, and a window that never ran at its stated
        // concurrency was read as an ordinary result of the gateway.
        spawn_failed: u.spawn_failed,
        rig_refused: u.rig_refused.max(0) as u64,
        budget_exceeded: u.budget_exceeded.max(0) as u64,
        // The subprocess never sends its raw samples back, only the percentiles it already
        // computed over them, so these are filled straight from the stats line rather than left
        // for a caller to (wrongly) derive from the now-empty `latencies_us` above.
        p50_us: percentile(u.p50_us),
        p99_us: percentile(u.p99_us),
    }
}

pub struct CellPerf {
    /// EVERY WINDOW THE CLIMB PROBED, in probe order. The whole of what this sweep produces.
    ///
    /// It used to carry four scalars beside these - `max_proxy` / `max_proxy_concurrency` and
    /// `sustained` / `sustained_concurrency` - which were this same set of windows summarised twice, once
    /// by a plateau search and once by a gate bisection. Both summaries are gone: `frontier.rs` reads the
    /// throughput answer at six declared tail-latency bounds off these rungs, so the sweep no longer
    /// decides anything and there is nothing left for a chosen ceiling to decide it with.
    ///
    /// `sustained_points` went with them. It existed to carry the bisection's extra windows as evidence
    /// for a number the bisection produced; there is no bisection and no such number, and these rungs are
    /// the evidence for every reading taken from them.
    pub points: Vec<crate::search::ProbedPoint>,
    pub interrupted: bool,
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
        // Nothing was probed, so there is no evidence to carry. The CELL's own `served` verdict says
        // why; a sweep with no rungs needs no separate reason of its own, and inventing one here would
        // be a second vocabulary for one fact.
        return CellPerf {
            points: Vec::new(),
            interrupted: false,
        };
    };
    let mut p = SweepProbe {
        cfg,
        path: path_for(cfg, ing, &id.egress),
        body: ing.body(&model_for(cfg, &id.egress)),
        headers: headers_for(cfg, ing, &id.egress),
    };
    // No start argument: the climb always begins at the floor. A start derived from the range made the
    // ladder arbitrary and made a WIDER range open with a HIGHER first probe, which is how a 1..65536
    // run once began by asking for 32768 concurrent connections.
    //
    // NO GATE ARGUMENT EITHER, and that is the change. The climb used to be handed the 20ms predicate so
    // it would keep going past the throughput plateau until the gate broke - a union of two stopping
    // rules for two summaries. There is one rule now (stop when requests start failing) and no summary:
    // every rung comes back and `frontier.rs` reads whichever bound a caller asks for.
    let result = search::climb_rungs_report(&mut p, lo, hi);
    CellPerf {
        points: result.points,
        interrupted: result.interrupted,
    }
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

/// Rungs at or below this drive too few sockets for draining to matter, and pausing after them is
/// pure schedule cost across a field sweep.
const STREAM_SETTLE_FREE_BELOW: u32 = 512;
/// One second of drain per 1,000 concurrent streams the last window drove.
const STREAM_SETTLE_MS_PER_1K: u64 = 1_000;
/// The backstop, and it is ONE TIME_WAIT GENERATION rather than a round number.
///
/// This was 10s, "deliberately shorter than TIME_WAIT" on the theory that the queues and descriptor
/// table drain in far less. The board disagreed: cells that had carried c=4,096 still failed at
/// c=4,107..4,193 after their 4-5s, and TIME_WAIT is exactly the residue `HostState` names as "the
/// one that takes tens of seconds to clear". Since the wait now ends on the OBSERVED counter, this
/// bounds only the pathological case - and 60s is the longest a closed socket can physically need.
/// Past it the host is not draining, it is broken, which is a finding rather than a longer wait.
const STREAM_SETTLE_MAX_MS: u64 = 60_000;
/// How often the drain condition is re-read. A /proc/net/sockstat read is one small file; 50ms keeps
/// the wait tight on the small rungs (where it now costs milliseconds instead of seconds) without
/// making the poll itself measurable.
const STREAM_SETTLE_POLL_MS: u64 = 50;
/// How far above this cell's own starting TIME_WAIT count still counts as drained.
///
/// A bench box is never perfectly idle: the mock and the load generator hold their own sockets, and
/// the gateway keeps a pool. Demanding an exact return to baseline would time out on every window
/// and turn the backstop into the schedule. The tolerance is a multiple of the count the cell STARTED
/// at, so it scales with whatever that particular box considers quiet rather than being a number
/// chosen here.
const STREAM_SETTLE_TW_TOLERANCE: u64 = 2;

thread_local! {
    /// The TIME_WAIT count this cell began with, times the tolerance: the level `settle_after_streams`
    /// waits to come back down to.
    ///
    /// PER CELL, NOT PER PROCESS. A run measures 36 cells back to back and the box's quiet level
    /// drifts across them (a gateway restarts, a previous cell's pool ages out). Sampling once at
    /// startup would have every later cell waiting on a number taken before any of them ran.
    /// `None` on a host with no /proc/net/sockstat, which is what makes the caller fall back to the
    /// clock instead of waiting on a signal that will never arrive.
    static STREAM_SETTLE_TW_BASELINE: std::cell::RefCell<Option<u64>> =
        const { std::cell::RefCell::new(None) };
}

/// Record what "quiet" means for the cell about to be measured. Called once, before its ladder.
pub fn arm_stream_settle_baseline() {
    let tw = HostState::sample().tw.map(settle_target);
    STREAM_SETTLE_TW_BASELINE.with(|b| *b.borrow_mut() = tw);
}

/// The TIME_WAIT level a settle waits to come back down to, given the count the cell started at:
/// the tolerance multiple of it, plus a fixed allowance so an idle box (0) still has a reachable
/// target. Pure, so the reachability the drain depends on can be asserted on the real rule.
fn settle_target(quiet_tw: u64) -> u64 {
    quiet_tw
        .saturating_mul(STREAM_SETTLE_TW_TOLERANCE)
        .saturating_add(64)
}

/// How many times the sustained ceiling may step down when it fails confirmation.
///
/// Each step narrows toward the cell's proven floor (see `stepdown_next`), so this bounds the walk
/// at a few steps below the bisection's answer - far enough to find a rung that genuinely holds,
/// short enough that a cell cannot spend its whole budget here.
const MAX_CEILING_STEPDOWNS: usize = 4;

/// WHY the sustained-stream search ended without a ceiling.
///
/// It exists because the absence used to be described by one hardcoded sentence no matter how the
/// search ended, and the five endings are not the same fact - one of them is OURS. Publishing "the
/// gateway did not hold the gate" for a window the RIG failed to take is the attribution error this
/// whole board is built to avoid, and it is invisible: the sentence reads like a measurement.
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
    /// A rung failed at a concurrency this same cell had ALREADY proved clean. Never the gateway's
    /// result: a gate that fails at 3,088 after passing 6,144 is measuring how far the rig has
    /// drained, not what the gateway can carry.
    RigContaminated { at: u32, proven: u32 },
    /// The gateway stopped serving after an overload and did not come back on its own. THIS ONE IS
    /// ABOUT THE GATEWAY, and it is a finding rather than a gap: aisix passed every rung to c=8,192,
    /// was pushed to c=16,384, and then failed seventeen consecutive windows including c=4,096 which
    /// it had just carried cleanly. A ceiling cannot be measured on a process that never recovers,
    /// but "it does not recover" is exactly what a reader wants to know, so it is published as the
    /// reason instead of an unexplained absence.
    ///
    /// `restart_cleared` records whether a restart brought it back - which is the difference between
    /// "wedged until restarted" and "wedged and stayed wedged", and the two are not the same claim.
    GatewayDidNotRecover {
        at: u32,
        proven: u32,
        restart_cleared: bool,
    },
    /// The restart that would have separated "the rig is dirty" from "the gateway is wedged" itself
    /// FAILED. Ours: the harness could not put the process back, so nothing about the gateway was
    /// learned here - and, unlike `RigContaminated`, the cause is not a host that had not drained.
    RestartFailed { at: u32, proven: u32 },
}

impl StreamStop {
    /// May the search fall back to confirming the cell's own proven-clean floor?
    ///
    /// ONLY WHERE THE HOST IS NOT THE SUSPECT. The floor fallback re-measures a rung on the same box
    /// that just failed several, so it is only meaningful when the failures were the gateway's or the
    /// budget's. `RigRanShort`, `WindowUnavailable` and `RigContaminated` all say the instrument is
    /// the variable, and a number taken on a host we have just accused would be exactly the
    /// attribution error the rest of this enum exists to prevent - with the flattering sign, because
    /// it would publish a figure where honesty published none. `RestartFailed` is excluded for the
    /// same reason: the harness could not even put the process back.
    ///
    /// `GatewayDidNotRecover` is excluded for the opposite reason: the process serving the fallback
    /// window is not the process the sweep measured, so whatever it carries is not this cell's
    /// ceiling. "It does not recover" is the finding, and it must not be overwritten by a number.
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
            | StreamStop::RigContaminated { .. }
            | StreamStop::RestartFailed { .. } => Absent::HarnessError,
            // A gateway that does not recover from overload is the GATEWAY's result, so it must not
            // be filed under our own faults - that would be the attribution error in reverse.
            //
            // Listed rather than caught by a wildcard: a new ending must be classified on purpose,
            // or a rig fault added later compiles silently into the gateway's results.
            StreamStop::GatewayDidNotRecover { .. }
            | StreamStop::FloorReached { .. }
            | StreamStop::SteppedRungFailed { .. }
            | StreamStop::BudgetExhausted => Absent::NotMeasured,
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
            StreamStop::RestartFailed { at, proven } => format!(
                "the bisection proved c={proved}, but re-measurement failed at c={at} although this \
                 cell had already carried c={proven}, and the restart that would have told the rig \
                 apart from the gateway failed - a harness fault, so nothing is published about the \
                 gateway here"
            ),
            StreamStop::BudgetExhausted => format!(
                "the bisection proved c={proved}, but that concurrency did not hold the stream gate \
                 on re-measurement and stepping down found none that did within {budget} attempts"
            ),
        }
    }
}

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
/// piece of judgement this whole metric turns on - can be unit-tested directly against fixed numbers.
/// A probe's own `probe()` drives a real subprocess load window and cannot be exercised that way in
/// this crate's unit tests (see `tests/end_to_end.rs`'s own note on why: under `cargo test` the
/// current exe is the test binary, not `otb`).
///
/// NOTHING IN THE MEASUREMENT PATH CALLS THIS ANY MORE. The sustained-throughput scalar it gated was
/// retired when the frontier replaced it (`record.rs`, `metric.rs`), and the only callers left are
/// this file's own unit tests. It is kept because the tests pin the README's stated bar - p99 under
/// the ceiling AND under 0.1% errors - and that bar is still the definition the frontier's
/// `served_cleanly` descends from; deleting it would delete the only executable statement of it.
///
/// Two claims were removed from this comment rather than left to mislead: it said the gate "is now
/// applied to the SAME windows the throughput sweep took, via `sustained_gate`" - there is no
/// `sustained_gate` function anywhere in the crate, and this one has no production caller - and it
/// cited `rigbound::is_rig_bound` as a live example, which was itself deleted (see `rigbound.rs`'s
/// own header). A doc comment describing an integration that does not exist sends the next reader
/// looking for code that was removed.
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
/// NOT `STREAM_FRAME_BUDGET`, and the difference is the point. That constant is how many frames the RIG
/// ASKS FOR - our own choice, legitimately a constant. This is how many the mock actually SENDS, which
/// is the mock's configuration and must be read from it. They coincide at 64 today, which is exactly why
/// nothing noticed the engine mirroring the mock's default instead of reading it: set
/// MOCK_STREAM_CHUNKS=128 and the mock sends 128 while the engine still believes 64, silently.
///
/// It matters now because the ceiling below is DERIVED from it. A frame count mirrored rather than read
/// would make that ceiling wrong by exactly the ratio of the two, and a derived bound built on a magic
/// number is still a magic number.
pub fn mock_stream_chunks() -> u32 {
    std::env::var("MOCK_STREAM_CHUNKS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        // Same reason `stream_pacing_interval_ms` rejects 0: the mock floors this at 1 (`chunks.max(1)`),
        // so a 0 here would describe a stream neither side produces.
        .filter(|v| *v > 0)
        .unwrap_or(64)
}

/// THE MOST FRAMES A SECOND THE MOCK CAN PHYSICALLY EMIT at this concurrency. Arithmetic, not a
/// measurement - we own the mock, so its ceiling is known rather than probed.
///
/// This replaces a MEASURED direct-to-mock reference, and the bench box's own core partitioning is why.
/// The two legs do not get the same machine:
///
///   through the gateway   the loadgen reads c streams from the gateway, and the GATEWAY drives c
///                         streams to the mock on its own cores - three core sets engaged.
///   direct to the mock    the loadgen BOTH drives and reads c streams on its cores; the gateway's sit
///                         idle - two core sets.
///
/// Removing the hop does not make the path leaner, it moves the driving half onto the already-busiest
/// component. So the direct leg is systematically SLOWER than the path it was meant to bound, and that
/// is structural rather than a fault - the partitioning is exactly right for comparing gateways.
///
/// Measured on the 2026-07-29 box at c=1024: the direct leg took 2.53-3.37s per window where this
/// arithmetic says 1.26s, delivering every frame with zero stalls - 38-50% of what the mock can emit.
/// The gateway leg reached 83%. NEITHER exceeded the real ceiling; the gateway only exceeded the weak
/// measurement of it, by 1.67x, and a chosen 1.5x factor discarded it for that. Seven gateway/metric
/// pairs on that board published nothing for the same reason.
///
/// The mock cannot emit frames faster than it sleeps, and both terms are declared: `mock_stream_chunks`
/// frames per stream, `stream_pacing_interval_ms` between them. It sleeps before every delta except the
/// first, so a stream lasts `(chunks - 1) * interval` and carries `chunks` frames. Nothing is chosen.
///
/// A RATE OVER THE MOCK'S WHOLE STREAM, NOT A BOUND ON WHAT A WINDOW CAN OBSERVE. `StreamWindow::fps`
/// counts every SSE event (the dialect's prelude, and any pings or re-framing a gateway adds) and a
/// lane stops reading once its content budget arrives, so a window can legitimately read a few
/// percent above this figure - more on a dialect with a longer prelude. That is why the ratio
/// published against it (`rigbound::headroom`) is reported as it is, above 1.0 included, and never
/// used to withhold a number.
pub fn mock_frame_ceiling_fps(concurrency: u32) -> f64 {
    let chunks = f64::from(mock_stream_chunks());
    let interval_s = stream_pacing_interval_ms() as f64 / 1000.0;
    let per_stream_s = (chunks - 1.0).max(1.0) * interval_s;
    if per_stream_s <= 0.0 || concurrency == 0 {
        return 0.0;
    }
    f64::from(concurrency) * chunks / per_stream_s
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
    pacing_interval_from(std::env::var("MOCK_STREAM_INTERVAL_MS").ok().as_deref())
}

/// The parse behind `stream_pacing_interval_ms`, over the variable's raw value (`None` when unset),
/// so the rule can be tested without mutating a process-global every other test reads.
fn pacing_interval_from(raw: Option<&str>) -> u64 {
    raw.and_then(|v| v.trim().parse().ok())
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
    /// `errored`, split by ATTRIBUTION. Always sums to `errored`.
    pub error_kinds: StreamErrorKinds,
    /// The host's socket/descriptor state sampled just BEFORE this window opened its connections.
    pub host_before: HostState,
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

    /// Why THIS ENGINE is at fault for the numbers in this window, if it is. `None` when the counts are
    /// arithmetically possible - which is not the same as the gateway having done well.
    ///
    /// A NUMBER THAT CANNOT HAPPEN IS OUR BUG, NEVER THE GATEWAY'S. That is the whole reason this
    /// exists, and it is the half of the old rig-ceiling comparison that was worth keeping. The other
    /// half - deciding that a legitimately fast gateway was "rig-bound" and withholding its number -
    /// is gone; see `rigbound.rs`. What survives is the case where the observation is not a
    /// measurement of anything: there is no gateway behaviour that produces it, so attributing it to
    /// the gateway (or to our rig's capacity) would publish a defect of ours as a finding about theirs.
    /// `Absent::HarnessError` is the only honest label, and `measurement.rs` already states that it and
    /// `RigLimited` must never be swapped.
    ///
    /// EXACT, WITH NO FACTOR AND NO TOLERANCE. Every clause below is arithmetic over counts this rig
    /// took itself, so there is nothing to tune and nothing to get wrong by a few percent:
    ///
    /// - Content frames above the expected content budget. The mock emits a DECLARED number of content
    ///   deltas per stream and `expected_content_frames` accumulates exactly that budget per surviving
    ///   lane, so counting more model output than the mock could have produced means we counted wrong.
    ///   Confirmed against the field data before relying on it: every instrumented window on the
    ///   2026-07-29 box delivered content exactly at budget (`content=64512/64512`, 24 of 24), never
    ///   above, and 1169 of the 1314 passing rungs on the 2026-07-28 board sat at exactly 1.0.
    /// - A non-finite or negative rate. `fps` divides counts by a wall clock; either is a broken clock
    ///   or a broken counter, and an infinity would win every peak search it appeared in.
    ///
    /// DELIBERATELY NOT CHECKED: `frames` above `expected_frames`. That one is legal and happens - a
    /// gateway that inserts pings or re-frames a translated stream spends more SSE events than the
    /// mock's own layout would, which is why the delivery gate counts content frames rather than
    /// events (Ledger RIG-11). Bounding the event count would fail honest gateways for their framing
    /// style, so the exact bound is available on content and only on content.
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
    stall_bound_for(stream_pacing_interval_ms())
}

/// The stall bound for one pacing interval. Saturating, because the interval is an operator-set
/// variable and an overflow would wrap to a tiny bound that calls every gap a stall.
fn stall_bound_for(interval_ms: u64) -> u64 {
    interval_ms
        .saturating_mul(STREAM_STALL_MULTIPLIER)
        .saturating_mul(1_000)
}

/// How many gaps in one lane's frame arrivals exceeded the stall bound.
///
/// GAPS, not the first frame's offset: time to first token is a latency the `Streaming` group already
/// publishes as a difference, and charging it here would make every stream stall on a gateway that is
/// merely slow to start rather than one that goes quiet mid-stream, which is what the README's rule
/// is about.
///
/// The bound is an argument (`stall_bound_us()` at the call site) because it is an environment read:
/// taken once per window rather than once per gap, and testable without touching the environment.
fn stalls_past(offsets: &[u64], bound_us: u64) -> u64 {
    offsets
        .windows(2)
        .filter(|w| w[1].saturating_sub(w[0]) > bound_us)
        .count() as u64
}

/// WHAT THE HOST LOOKED LIKE WHEN A WINDOW RAN.
///
/// A rung that fails at a concurrency the same cell already carried cleanly is either the gateway
/// changing its mind or the host not having recovered, and no amount of staring at pass/fail can
/// tell those apart. These are the counters that separate them, sampled immediately before the
/// window opens its connections:
///
///   * `tw` - sockets in TIME_WAIT across the box (`/proc/net/sockstat`). The residue a previous
///     window leaves behind, and the one that takes tens of seconds to clear.
///   * `tcp_inuse` / `tcp_alloc` - live and allocated TCP sockets, so a leak shows as a floor that
///     never returns rather than a spike that does.
///   * `fds` - open descriptors in THIS process (`/proc/self/fd`), which is where a loadgen holding
///     a reader per stream would show up.
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

/// WHY a stream errored, not just THAT it did. One `errored` count cannot answer the only question
/// worth asking about a failed rung - was it us or them - and that ambiguity is why the
/// sustained-stream defect went undiagnosed: "636 of 8192 streams errored" reads like a gateway
/// collapse and reads exactly the same when the peer reset every one of them for a reason of ours.
///
/// These are attribution classes, not error codes. `Refused`/`Reset` is the peer declining a
/// connection it was asked for; `Status` is the peer answering and saying no; `NoResponse` is the
/// peer taking the connection and never producing a response head; `NoFrames` is a 2xx
/// that never produced an event; `Malformed`/`NotEventStream` is the peer speaking something that is
/// not SSE. Rig-side ends (`RigExhausted`, `RigRefused`) never reach here - `stream_window` discards
/// the whole window for those, so they cannot be laundered into a gateway's error rate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StreamErrorKinds {
    /// The peer refused, reset, or was unreachable - the connection never carried a request.
    pub connect_failed: u64,
    /// The peer answered with a non-2xx status.
    pub status: u64,
    /// The connection was made but no response head ever arrived (or none that parsed): the lane
    /// timed out or was closed before the peer said anything. Not `status` - that bucket says the
    /// peer ANSWERED with an error, and a gateway that never answered must not read as one that did.
    pub no_response: u64,
    /// A 2xx that delivered no event at all.
    pub no_frames: u64,
    /// The peer answered 2xx but did not speak a well-formed event stream.
    pub not_event_stream: u64,
}

impl StreamErrorKinds {
    pub fn total(&self) -> u64 {
        self.connect_failed
            + self.status
            + self.no_response
            + self.no_frames
            + self.not_event_stream
    }
    fn add(&mut self, o: &crate::http::SseOutcome) {
        use crate::http::SseEnd;
        // ORDER MATTERS AND MIRRORS `stream_errored`: a lane that never connected has no status and
        // no frames, so testing frames first would file every refused connection as "no frames" and
        // erase the distinction this type exists for.
        if matches!(o.end, SseEnd::ConnectionFailed(_)) {
            self.connect_failed += 1;
        } else if o.status.is_none() {
            self.no_response += 1;
        } else if !o.status.is_some_and(|s| (200..300).contains(&s)) {
            self.status += 1;
        } else if matches!(o.end, SseEnd::Malformed(_) | SseEnd::NotAnEventStream(_)) {
            self.not_event_stream += 1;
        } else {
            self.no_frames += 1;
        }
    }
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

/// LET THE HOST DRAIN BETWEEN STREAM WINDOWS, SCALED TO WHAT THE LAST ONE DROVE.
///
/// A window at c=6,176 leaves that many sockets working through FIN_WAIT/TIME_WAIT and that many
/// descriptors coming back. The very next window opens its own connections into whatever is left, so
/// a rung measured immediately after a big one is measuring the residue as much as the gateway.
///
/// This is not a hypothesis. On the 2026-07-31 board the sustained-stream bisection failed to
/// converge on 6 cells across 4 independent gateways, and every one carried the same impossible
/// signature: a rung failing at a concurrency THAT SAME CELL had already carried cleanly minutes
/// earlier (busbar failed c=3,088 after passing c=4,096 and c=6,144 with zero errors; agentgateway
/// failed c=1,357 after c=2,048; one-api failed c=170 after c=256). The engine already knew this
/// effect existed - `stream_rig_ceiling` sleeps for exactly this reason, in a comment that names
/// draining sockets - but it settled only AFTER the ladder, protecting the reference measurement and
/// not the ladder's own re-measurements, which are the numbers that actually get published.
///
/// WAIT FOR THE HOST TO SAY IT HAS DRAINED, RATHER THAN GUESSING HOW LONG THAT TAKES.
///
/// This slept `concurrency * 1s/1000`, capped at 10s. A duration is the wrong shape for this: no
/// constant is right for every gateway and every rung, and the field showed it failing in BOTH
/// directions at once. Too long: 1,647 windows above the free-below threshold, averaging 3.3s of
/// unconditional sleep, is 91.8 MINUTES of pure waiting in a single field run. Too short: cells that
/// had carried c=4,096 still failed at c=4,107..4,193 after their 4-5s, which is the residue this
/// function exists to absorb, and the old cap was deliberately set BELOW one TIME_WAIT generation on
/// the theory that the queues drain much faster. The data says otherwise.
///
/// The signal was already being collected and never acted on. `HostState` samples `tw` - sockets in
/// TIME_WAIT from /proc/net/sockstat - immediately before every window, and the comment on it names
/// this exact purpose: telling "the gateway changed its mind" apart from "the host had not
/// recovered". So the wait now ENDS when the host is observably back, which is short on a small rung
/// and long on a large one without either being chosen in advance.
///
/// THE 60s BACKSTOP IS ONE TIME_WAIT GENERATION, not a round number: it is the longest recovery a
/// closed socket can physically need. Past it the host is not draining, it is broken, and that is a
/// finding to report rather than a wait to extend.
fn settle_after_streams(concurrency: u32) {
    if concurrency <= STREAM_SETTLE_FREE_BELOW {
        return;
    }
    let by_clock = || {
        std::thread::sleep(std::time::Duration::from_millis(settle_clock_ms(
            concurrency,
        )))
    };
    let baseline = STREAM_SETTLE_TW_BASELINE.with(|b| *b.borrow());
    let Some(target) = baseline else {
        // No baseline means no /proc to read (a non-Linux host, or the first call). Fall back to the
        // proportional sleep: an unobservable host still needs SOME pause, and the old behaviour is
        // the honest default when the condition cannot be evaluated.
        by_clock();
        return;
    };
    let started = std::time::Instant::now();
    let deadline = std::time::Duration::from_millis(STREAM_SETTLE_MAX_MS);
    loop {
        let now = HostState::sample();
        match now.tw {
            // Drained: TIME_WAIT is back within the tolerance of where this cell started. The
            // tolerance exists because a box is never perfectly idle - the mock and the load
            // generator hold their own sockets - so demanding an exact return would always time out.
            Some(tw) if tw <= target => return,
            // The counter stopped being readable mid-sweep. Waiting on a signal that no longer
            // exists would burn the whole backstop on every window, so this falls back to the clock.
            None => {
                by_clock();
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

/// The clock-only settle: proportional to what the last window drove, free at or below
/// `STREAM_SETTLE_FREE_BELOW`, capped at `STREAM_SETTLE_MAX_MS`. Used only where the drain cannot be
/// observed, and pure so the free/scaled/capped shape is asserted on the rule the settle applies.
fn settle_clock_ms(concurrency: u32) -> u64 {
    if concurrency <= STREAM_SETTLE_FREE_BELOW {
        return 0;
    }
    (u64::from(concurrency) * STREAM_SETTLE_MS_PER_1K / 1000).min(STREAM_SETTLE_MAX_MS)
}

/// Drive `concurrency` concurrent streams against `addr` and read each one to the frame budget.
///
/// `None` means the window never ran at the requested concurrency - the OS refused a lane - which is
/// a RIG limit exactly as `SweepProbe`'s `spawn_failed` is: nothing about the gateway was learned, so
/// the search must stop rather than read the shortfall as a turnover.
///
/// IN-PROCESS LANES, unlike `load_window`'s pinned child: one tokio task per lane on a runtime built
/// for the window (see the note inside). A stream lane spends its life asleep between the mock's
/// paced deltas rather than saturating a core, so it does not contend for the orchestrator's CPU the
/// way the request generator would. What this does NOT get is the generator's core pinning, and at
/// concurrencies high enough to saturate the box that is a real limitation on the absolute
/// frames/sec. The mock's own rate beside it is ARITHMETIC (`mock_frame_ceiling_fps`), not a window
/// taken with this function, and it is published as a ratio (`rigbound::headroom`) - nothing is
/// withheld on it.
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
    // SHARED, NOT COPIED PER LANE. Three owned clones per lane is ~100k allocations at c=32,768,
    // made on the spawn loop while earlier lanes are already streaming - which lengthens the ramp
    // and competes with those lanes for the allocator. A refcount bump costs neither.
    let path: std::sync::Arc<str> = path.into();
    let body: std::sync::Arc<str> = body.into();
    let headers: std::sync::Arc<[(String, String)]> = headers.into();

    // SAMPLED HERE, before a single connection is opened, because the question this answers is what
    // the window INHERITED - not what it left behind. Sampling after would measure this window's own
    // sockets and could never distinguish a dirty start from a busy finish.
    let host_before = HostState::sample();

    let (outcomes, panicked, elapsed_s): (Vec<crate::http::SseOutcome>, usize, f64) =
        rt.block_on(async move {
            // THE CLOCK STARTS BEFORE THE FIRST LANE IS SPAWNED, so the ramp is INSIDE the
            // denominator. Lanes start streaming the moment they are spawned, so a clock started
            // after the spawn loop counted every frame the early lanes delivered during the ramp
            // against an interval that excluded it - which inflates fps(), most at exactly the high
            // rungs where the ramp is longest. Including the ramp can only understate the rate (the
            // conservative direction), and the shared `Arc`s below keep the ramp itself short.
            let started = std::time::Instant::now();
            let mut lanes = Vec::with_capacity(concurrency as usize);
            for _ in 0..concurrency {
                let path = std::sync::Arc::clone(&path);
                let body = std::sync::Arc::clone(&body);
                let headers = std::sync::Arc::clone(&headers);
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
        error_kinds: StreamErrorKinds::default(),
        host_before,
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
    // Read once per window: it is an environment read, and it cannot change mid-window.
    let stall_bound = stall_bound_us();
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
        w.stalls += stalls_past(&o.frame_offsets_us, stall_bound);
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

/// The next rung the stream step-down probes after `ceiling` failed confirmation. Pure, so the rule
/// `sweep_streams_cell` applies is the rule the tests assert.
///
/// Bisects between the known-good floor (the top of the cell's own uncontaminated ascending prefix,
/// or the search's lower bound) and the failed rung; halves only when there is no bracket - the
/// confirmation failed at the known-good rung itself, so nothing below it is known.
fn stepdown_next(proven_clean: u32, lo: u32, ceiling: u32) -> u32 {
    let known_good = if proven_clean > 0 {
        proven_clean.max(lo)
    } else {
        lo
    };
    if known_good < ceiling {
        let mid = known_good + (ceiling - known_good) / 2;
        // A bracket one rung wide bisects to its own floor; stepping to `ceiling` would loop
        // forever, so the floor is the honest next probe.
        if mid >= ceiling {
            known_good
        } else {
            mid
        }
    } else {
        ceiling / 2
    }
}

/// Whether a stepped rung failing at `at` can be the rig's residue rather than the gateway: only at
/// or under a rung this cell already carried cleanly, and only where that rung drove enough sockets
/// for draining to exist at all (`STREAM_SETTLE_FREE_BELOW`). Anywhere else a failure is a failure -
/// widening this would relabel real gateway failures as ours, which flatters the subject.
fn stream_contaminated(proven_clean: u32, at: u32) -> bool {
    proven_clean > STREAM_SETTLE_FREE_BELOW && at <= proven_clean
}

/// What a completed restart says about a rung that failed under a proven one: cleared by the
/// restart means the old process was wedged (the GATEWAY's finding); not cleared means a fresh
/// process still fails, which leaves the host as the variable (ours).
fn restart_stop(restart_cleared: bool, at: u32, proven: u32) -> StreamStop {
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

/// May the stream search fall back to confirming its proven floor? Only when there is one, when the
/// way the search ended leaves the host unaccused (`StreamStop::floor_fallback_ok`), and when that
/// floor has not already failed a full confirmation of its own in this search.
fn floor_fallback_due(proven_clean: u32, floor_refuted: bool, stop: StreamStop) -> bool {
    proven_clean > 0 && !floor_refuted && stop.floor_fallback_ok()
}

thread_local! {
    /// Set when a restart INSIDE a cell's metric groups failed (the stream search's attribution
    /// restart, or the memory group's return to rest), so the grid that called it can refuse to
    /// measure the rest of the run against a process the harness can no longer vouch for - the same
    /// rule `run_grid_streaming` applies to its own failed restart. The search runs on the grid's
    /// thread (the metric groups are called synchronously from it), so a thread-local reaches it
    /// without threading a flag through every metric group.
    static STREAM_RESTART_FAILURE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

pub(crate) fn note_stream_restart_failure(why: &str) {
    STREAM_RESTART_FAILURE.with(|f| *f.borrow_mut() = Some(why.to_string()));
}

/// Read, WITHOUT clearing, a restart failure the stream search recorded on this thread. For
/// `metric::process_cell_with`, which must not run the cell's remaining groups against a process the
/// harness can no longer vouch for; the grid still `take`s it afterwards to poison the rest of the run.
pub(crate) fn stream_restart_failure_pending() -> Option<String> {
    STREAM_RESTART_FAILURE.with(|f| f.borrow().clone())
}

/// Take (and clear) a restart failure the stream search recorded on this thread.
fn take_stream_restart_failure() -> Option<String> {
    STREAM_RESTART_FAILURE.with(|f| f.borrow_mut().take())
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
    /// `errored`, split by attribution - see `StreamErrorKinds`. Rides on the POINT, not just the
    /// window, because the sweep array is the only record that survives into the snapshot, and a
    /// failed rung whose cause is not recorded there cannot be diagnosed after the box is gone.
    pub error_kinds: StreamErrorKinds,
    /// The host state this rung was measured against - see `HostState`. On the POINT because the
    /// sweep array is what survives into the snapshot.
    pub host_before: HostState,
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
            // THE BREAKDOWN, always emitted (zeros included) so a clean rung and an unrecorded one
            // are distinguishable in the snapshot.
            "stream_errors_connect_failed": self.error_kinds.connect_failed,
            "stream_errors_status": self.error_kinds.status,
            "stream_errors_no_response": self.error_kinds.no_response,
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
/// `bisect_ceiling`, not `saturation_plateau`: this is a monotone pass/fail gate in concurrency. Once
/// enough concurrent streams are in flight that frames start arriving late or short, adding more does
/// not bring them back.
pub fn sweep_streams_cell(cfg: &RunConfig, id: &CellId, lo: u32, hi: u32) -> CellStreams {
    // WHAT "DRAINED" MEANS FOR THIS CELL, read before it drives anything. Every settle in this
    // ladder waits to come back to this level, so it has to be taken while the box is as quiet as it
    // will be - and re-taken per cell, because a run measures 36 of them back to back and the quiet
    // level drifts as gateways restart and pools age out.
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
                /* THE TOP OF THIS CELL'S OWN UNCONTAMINATED ASCENDING PREFIX - every rung from the
                   first up to and including it passed, before anything in this cell had failed. It is
                   the only concurrency figure in scope that is certainly NOT an artefact of a busy
                   host, because nothing had been driven hard yet when it was taken. Used below as the
                   line under which a failure cannot honestly be the gateway's. */
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
                // Whether `proven_clean` itself has already had a FULL confirmation and lost it.
                // The floor fallback below exists to give that rung the vote the search never
                // gave it; a rung that already had the vote and failed must not get a second one.
                let mut floor_refuted = false;
                /* A STEPPED RUNG WHOSE SEED WINDOW FAILS MUST STEP DOWN AGAIN, NOT END THE SEARCH.
                   plano `anthropic>anthropic` is what this costs: the ascending sweep carried c=64,
                   the bisection settled on c=79, confirmation failed, the step-down bisected to
                   c=71, its first window failed - and the search stopped there, with every
                   concurrency between 64 and 71 untried and most of the step-down budget unspent.
                   The cell published an absence where a number was almost certainly available.

                   What it must NOT do is give that rung its confirmation windows anyway: a rung that
                   could not seed has not earned a vote, and crediting it is how a gate-failing window
                   folds into a published figure. So the failed seed buys another step-down and
                   nothing else - the rung is abandoned, the bracket narrows, and the search carries
                   on with the budget it has left. */
                let mut seed_failed = false;
                for _ in 0..MAX_CEILING_STEPDOWNS {
                    if seed_failed {
                        // The rung below is abandoned unconfirmed; fall straight to another
                        // step-down rather than voting on a window that never held.
                        seed_failed = false;
                    } else {
                    let mut held = 1usize; // the bisection's own winning window is a real vote
                    let mut total = 1usize;
                    let mut rates = vec![first_fps];
                    for _ in 1..crate::search::WINDOWS_PER_RUNG {
                        // The previous window at this same concurrency is still draining off the host.
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
                        stop = StreamStop::RigRanShort {
                            measured: total,
                            wanted: crate::search::WINDOWS_PER_RUNG,
                        };
                        break;
                    }
                    if ceiling == proven_clean {
                        floor_refuted = true;
                    }
                    }
                    /* STEP DOWN INSIDE THE BRACKET, NOT BY HALVING.
                       Halving throws away everything the ascending sweep and the bisection just
                       established. busbar proved every rung to c=4,096, bisected to c=6,176, failed
                       to confirm it - and then halved to c=3,088, a concurrency it had already
                       carried cleanly, discarding the entire 4,096..6,176 bracket the search had
                       spent fifteen rungs narrowing. The true ceiling was never below 4,096; it was
                       just under 6,176 (a later run confirmed 5,120).

                       The known-good floor is the top of this cell's own uncontaminated ascending
                       prefix, falling back to the search's lower bound. Bisecting between it and the
                       failed rung keeps every rung the search already paid for and converges toward
                       the boundary instead of collapsing away from it.

                       Halving survives only as the fallback for when there IS no bracket - the
                       confirmation failed at the known-good rung itself, so nothing below it is
                       known and there is no information to bisect. */
                    let next = stepdown_next(proven_clean, lo, ceiling);
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
                                /* A RUNG CANNOT FAIL BELOW ONE IT HAS ALREADY PASSED, so when it does,
                                   the gateway is not what changed. `proven_clean` is the top of THIS
                                   cell's own uncontaminated ascending prefix; a failure at or under it
                                   is the rig, and filing it as `SteppedRungFailed` charged the gateway
                                   for our own undrained host on 6 cells of the 2026-07-31 board.

                                   One full-cap retry first, because the cheap explanation - not enough
                                   drain - is also the likeliest, and it costs one window to rule out.
                                   If it holds this time the search simply carries on and publishes a
                                   number, which is the outcome all of this is for. */
                                /* AND ONLY WHERE THE MECHANISM CAN ACTUALLY EXIST. Draining sockets
                                   explain a rung failing under a proven one at c=3,088; they explain
                                   nothing at c=1 under c=2, where there is no residue to speak of and
                                   a failure is simply a failure. Gating on the same threshold the
                                   settle uses keeps this from becoming a universal excuse that
                                   relabels every real gateway failure as our fault - the mirror image
                                   of the bug it fixes, and the more dangerous of the two, because it
                                   flatters the subject. */
                                let contaminated = stream_contaminated(proven_clean, ceiling);
                                let recovered = if contaminated {
                                    eprintln!(
                                        "streams: c={ceiling} failed the gate although this cell already \
                                         carried c={proven_clean} cleanly - settling and re-taking the window"
                                    );
                                    std::thread::sleep(std::time::Duration::from_millis(STREAM_SETTLE_MAX_MS));
                                    match stream_window(cfg.gateway_addr, &p.path, &p.body, &p.headers, p.dialect, ceiling) {
                                        Some(w2) => {
                                            let passed2 = streams_gate_passes(&w2);
                                            p.points.push(point_of(&w2, passed2));
                                            if passed2 { first_fps = w2.fps(); }
                                            passed2
                                        }
                                        // THE RETRY COULD NOT RUN, which is the rig, not a second
                                        // failure of the gateway. Falling through to the restart
                                        // would let the restart's own elapsed time drain the host,
                                        // pass the post-restart window, and publish
                                        // `GatewayDidNotRecover` against the gateway for a window
                                        // we never took.
                                        None => {
                                            stop = StreamStop::WindowUnavailable { at: ceiling };
                                            break;
                                        }
                                    }
                                } else {
                                    false
                                };
                                /* A SETTLE DID NOT BRING IT BACK, SO ASK WHETHER ANYTHING WILL.
                                   The remaining explanations are opposite: our rig is still dirty,
                                   or the GATEWAY stopped serving and is not coming back on its own.
                                   Restarting is the one experiment that separates them, and the
                                   harness already owns this process's lifetime - the memory group
                                   stops and starts it every cell, and the probe loop restarts it
                                   mid-grid for a refused connection.

                                   A number is deliberately NOT published on the far side of this.
                                   Whatever a restarted gateway carries is a fact about a fresh
                                   process, not about the one that just served fifteen rungs, and
                                   quietly folding it into the same sustained figure would publish
                                   two different processes as one measurement. What gets published
                                   instead is the finding: it did not recover. */
                                if !recovered && contaminated {
                                    let mut restart_failed = false;
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
                                                // A FAILED RESTART IS OURS, AND IT POISONS WHAT
                                                // FOLLOWS. The process may be down or up but
                                                // half-configured; `run_grid_streaming` refuses to
                                                // measure later cells against it, exactly as for its
                                                // own failed restart.
                                                Err(e) => {
                                                    eprintln!("streams: the gateway could not be restarted: {e}");
                                                    note_stream_restart_failure(&e);
                                                    restart_failed = true;
                                                    false
                                                }
                                            }
                                        }
                                        // No declared relaunch means the harness does not own this
                                        // process and must not bounce it; the honest reading stays
                                        // the rig-side one it already had.
                                        None => false,
                                    };
                                    /* THE RESTART IS THE ATTRIBUTION TEST, and its two outcomes
                                       point at opposite culprits.

                                       Cleared by a restart: the process that had been serving was
                                       wedged, a fresh one is not, and the host was the same
                                       throughout - so it is the GATEWAY that did not recover.

                                       NOT cleared: a brand-new gateway process still cannot carry a
                                       rung this cell already carried. The gateway is the thing that
                                       just changed and the failure did not, which leaves the host as
                                       the variable - so this stays OURS. Calling it a gateway
                                       failure here would be the flattering direction of the same
                                       error, and the one nobody would catch. */
                                    stop = if restart_failed {
                                        StreamStop::RestartFailed { at: ceiling, proven: proven_clean }
                                    } else {
                                        restart_stop(restart_cleared, ceiling, proven_clean)
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
                                // A seed that HOLDS supersedes an earlier rung's seed failure: if
                                // the budget runs out from here, that is how the search ended.
                                stop = StreamStop::BudgetExhausted;
                            }
                        }
                        None => {
                            stop = StreamStop::WindowUnavailable { at: ceiling };
                            break;
                        }
                    }
                }
                /* THE FLOOR THE SEARCH ALREADY PROVED IS A RESULT, NOT A LEFTOVER.
                
                   The step-down budget converges toward `proven_clean` without ever re-testing it:
                   busbar openai>openai carried c=4,096 on the ascending sweep, bisected to c=5,652,
                   lost both confirmation windows, and spent all four step-downs at 4,874 / 4,485 /
                   4,290 / 4,193 - every one of them ABOVE the rung it had already carried. The cell
                   then published nothing, on a board where the same gateway is the subject. Three
                   busbar cells, and seven across the field, read `unconfirmed` for this reason.
                
                   "We could not confirm 5,652" and "we know nothing" are different statements. The
                   first is true; the second is what an absence says. So when the search is otherwise
                   out of budget, the floor gets the confirmation the bisected rung got: a full set of
                   windows, the same majority rule, the same median-of-holds. It publishes a
                   CONSERVATIVE number - the ceiling is somewhere above it - and that is the honest
                   shape of what the search learned.
                
                   It costs WINDOWS_PER_RUNG windows and only on the failure path. It cannot inflate a
                   result: `proven_clean` is a rung this cell drove cleanly during its own ascent, and
                   it still has to hold a majority now to be published at all - and only if it has not
                   ALREADY failed a full confirmation in this search (`floor_refuted`): when the
                   bisection's answer IS the floor, the floor has had its vote, and re-voting it after
                   the rungs below it failed would publish a ceiling the evidence does not support. */
                if winner.is_none() && floor_fallback_due(proven_clean, floor_refuted, stop) {
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
                    // THE REASON IS THE ONE THAT ACTUALLY HAPPENED, not one sentence for five outcomes.
                    //
                    // This was a single hardcoded string - "stepping down found none that did within N
                    // attempts" - emitted for EVERY way the search ends without a winner. There are five,
                    // and they are not the same fact:
                    //
                    //   * the rig could not complete its windows          (OUR failure, and the code says
                    //                                                      so five lines up before
                    //                                                      breaking with this message)
                    //   * the step-down floor was reached
                    //   * a stepped rung's first window failed the gate
                    //   * the window could not be taken at all
                    //   * the step-down budget was genuinely exhausted
                    //
                    // Reporting a RIG shortfall as "the gateway did not hold the gate" is the precise
                    // error this board exists to avoid, and it was doing it in a sentence that reads like
                    // a measurement. It also made two opposite findings render identically: busbar
                    // delivering ZERO frames on re-measurement and litellm-rust degrading gracefully near
                    // a real ceiling of ~3,144 streams got the same words, so the site could not tell a
                    // gateway's collapse from our own instrument giving up.
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
// `cpu_fps` was "peak SSE frames/sec", found by a `saturation_plateau` over a saturating curve. It is
// gone, and the reason is that the field data says no reading of it was defensible. Across the 16 cells
// on the 2026-07-29 board that published both it and `streams_sustained_fps` - the rate at the PROVEN
// delivery boundary - it fell into exactly three regimes:
//
//   4 of 16 INVERTED. agentgateway anthropic>openai read cpu_fps 6,949 against a gated 12,575 - a
//     "peak" 45% BELOW a number the bisection proved. A maximum under a proven boundary is not a
//     maximum, and this is the same defect `saturation_plateau` caused on the throughput lane: the
//     plateau search stopped on three flat rungs while the bisection kept going over the same windows.
//   5 of 16 REDUNDANT. portkey and tensorzero came in at 0.98-0.99x of `streams_sustained_fps`. The
//     same number, published twice, under two names.
//   7 of 16 ABOVE THE GATE. one-api 2.47x, plano 1.73x. A frame rate achieved at a concurrency where
//     the delivery gate did NOT hold - which means it was measured while dropping frames or stalling.
//     Publishing that as a throughput figure credits a gateway for frames its users never received.
//
// `streams_sustained_fps` is the honest version of the same quantity: the frame rate at a concurrency
// proven to deliver every content frame with no stall past the pace. It stays.

/// Whether `clean` direct-to-mock windows are enough to stand behind as a reference: a full
/// `WINDOWS_PER_RUNG`, the same bar the number it would judge was held to.
fn reference_has_full_set(clean: usize) -> bool {
    clean >= crate::search::WINDOWS_PER_RUNG
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
    // NOT ON THE LIVE PATH. `suite::stream_rig_ceiling` calls `mock_frame_ceiling_fps` - a pure arithmetic
    // derivation from the mock's declared pacing - and nothing outside this file's tests calls this
    // function. So the protocol described below (median of WINDOWS_PER_RUNG clean windows, settle first,
    // IMPOSSIBLE_FACTOR guard) is NOT what any published streams_sustained_fps was checked against; the
    // live reference is one-shot arithmetic with none of those safeguards. Kept because the arithmetic
    // ceiling has no measured cross-check at all, and this is the only implementation of one - but a
    // reader must not infer from the detail here that it is running.
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
    // A FULL SET, NOT MERELY A NON-EMPTY ONE.
    //
    // Taking the median of three windows was supposed to stop one unlucky window deciding a ceiling,
    // and it did not: `median` returns a value whenever the vec is non-empty, so ONE clean window out
    // of three still produced a published reference - the exact single-sample bar the change was
    // written to remove. And the direction matters, because an UNDERSTATED reference is what suppresses
    // a gateway's real number: the observation then clears it by more than IMPOSSIBLE_FACTOR and a
    // measured figure is thrown away as unvouchable. Seven gateway/metric pairs on the 2026-07-29 board
    // published nothing that way.
    //
    // So the bar here is the bar everywhere else - `confirm_ceiling` and `stream_ceiling_confirmed`
    // both require WINDOWS_PER_RUNG - and falling short says so with the count, rather than quietly
    // handing back a ceiling one window wide.
    if !reference_has_full_set(clean.len()) {
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
                if why.contains(NO_CONNECTION_PREFIX) {
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
                // Cleared first so a failure recorded by an earlier caller on this thread cannot be
                // charged to this grid.
                let _ = take_stream_restart_failure();
                let (m, s, t) = metric::process_cell_with(&ctx, metrics);
                if let Some(e) = take_stream_restart_failure() {
                    eprintln!(
                        "[cell {done}/{total}] {id}: a restart inside this cell's measurement failed: {e} - refusing to measure the rest of the grid against an unvouched process"
                    );
                    restart_poisoned = Some(format!(
                        "a restart of the gateway inside a cell's measurement failed ({e}), so the harness can no longer vouch for the gateway's state; measuring on would publish our failure as the gateway's"
                    ));
                }
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

#[cfg(test)]
#[path = "tests/run_fixture.rs"]
mod fixture;
#[cfg(test)]
pub(crate) use fixture::test_fixture;

#[cfg(test)]
#[path = "tests/run_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/run_stream_stop_tests.rs"]
mod stream_stop_tests;

#[cfg(test)]
#[path = "tests/run_contamination_tests.rs"]
mod contamination_tests;

#[cfg(test)]
#[path = "tests/run_stream_error_kind_tests.rs"]
mod stream_error_kind_tests;

#[cfg(test)]
#[path = "tests/run_stepdown_tests.rs"]
mod stepdown_tests;

#[cfg(test)]
#[path = "tests/run_gateway_recovery_tests.rs"]
mod gateway_recovery_tests;

#[cfg(test)]
#[path = "tests/run_restart_attribution_tests.rs"]
mod restart_attribution_tests;

#[cfg(test)]
#[path = "tests/run_search_simulator.rs"]
mod search_simulator;
