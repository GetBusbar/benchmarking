// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// A gateway manifest, as DATA.
//
// Identity is declared ONCE, not spelled out separately per reader (RSS, HWM, stop). If each hook
// named its own container/process pattern they could drift — e.g. RSS reading one process while HWM
// sums it plus its children — publishing two different populations for the same gateway.

use serde::{Deserialize, Serialize};

/// How the gateway runs, and therefore how its process tree is found. This is the single declaration
/// every memory reader and the stop path derive from.
/* AN UNKNOWN KEY IS A SILENT NO-OP, AND THAT COSTS A RUN.
`Runtime::Docker` has no `image` field (the image lives on `launch`); without `deny_unknown_fields`
a stray `runtime.image` parses cleanly, is read by nothing, and silently changes nothing — a box
could run the wrong image version under a mislabelled run. A field nobody reads is a typo or a
stale key, and both should fail at load where the fix is cheap, not at publish. */
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Runtime {
    /// A container. The root pid comes from the container runtime, and the tree walk starts there.
    Docker {
        /// The name as the manifest declares it, stable across runs.
        container: String,
        /// THE RUN THAT OWNS THIS CONTAINER, appended to the declared name to form the real one.
        ///
        /// Without this, `--name` is the declared name alone — identical across runs of the same
        /// gateway on the same box. Two overlapping runs would then collide on one container name,
        /// and a retry's `docker rm -f` could delete the other run's container mid-measurement.
        ///
        /// Not in any manifest, never serialized back: assigned per invocation
        /// (`Runtime::scoped_to_run`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_scope: Option<String>,
    },
    /// A process started directly on the box, located by a match against its command line.
    Native { proc_match: String },
}

impl Runtime {
    /// The one identity string, whatever the kind. For a container this is the RUN-SCOPED name —
    /// the same name used to create, measure, and remove it.
    pub fn identity(&self) -> String {
        match self {
            Runtime::Docker {
                container,
                run_scope: Some(scope),
            } => format!("{container}-{scope}"),
            Runtime::Docker { container, .. } => container.clone(),
            Runtime::Native { proc_match } => proc_match.clone(),
        }
    }

    /// The identity as the MANIFEST spells it, without a run scope. For validation messages and the
    /// `otb.gateway` label only — never for naming, finding, or removing a container (use `identity`).
    pub fn declared_identity(&self) -> &str {
        match self {
            Runtime::Docker { container, .. } => container,
            Runtime::Native { proc_match } => proc_match,
        }
    }

    /// Which run owns this identity, if it has been scoped to one.
    pub fn run_scope(&self) -> Option<&str> {
        match self {
            Runtime::Docker { run_scope, .. } => run_scope.as_deref(),
            Runtime::Native { .. } => None,
        }
    }

    /// Bind this identity to one run, so concurrent runs on a shared host cannot name (and therefore
    /// cannot remove) each other's containers.
    ///
    /// A NATIVE identity is returned UNCHANGED deliberately: `proc_match` matches a command line the
    /// gateway itself produces and contains no run id. A native gateway's isolation comes from the
    /// port it binds instead.
    pub fn scoped_to_run(&self, run_id: &str) -> Runtime {
        match self {
            Runtime::Docker { container, .. } => Runtime::Docker {
                container: container.clone(),
                run_scope: sanitize_run_scope(run_id),
            },
            Runtime::Native { .. } => self.clone(),
        }
    }

    pub fn is_docker(&self) -> bool {
        matches!(self, Runtime::Docker { .. })
    }
}

/// A run id reduced to what a container name accepts (`[a-zA-Z0-9_.-]`); a rejected name is a
/// launch that never happens. `None` if nothing survives, so an unusable id leaves the name
/// unscoped rather than trailing a bare separator.
fn sanitize_run_scope(run_id: &str) -> Option<String> {
    let cleaned: String = run_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        .collect();
    let cleaned = cleaned.trim_matches(['.', '-', '_']).to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Command-line fragments too generic to name one process. Matching is a SUBSTRING against every
/// full command line on the box (`supervise::select_matches`), so `sh` or `node` would select a
/// crowd, and which member gets signalled or summed is a coin toss reported as the gateway's.
const GENERIC_PROC_MATCHES: [&str; 14] = [
    "sh", "bash", "python", "python3", "node", "java", "docker", "server", "proxy", "gateway",
    "main", "app", "run", "start",
];

/// The shortest a `proc_match` may be; every real entrant declares a path or full binary name.
const MIN_PROC_MATCH_LEN: usize = 8;

/// Why a declared `proc_match` cannot be trusted to name exactly one process, or `None` if it can.
///
/// Checked at manifest level, not just at match time: a generic pattern's damage is silent (wrong
/// process stopped, or a bystander's memory published as the gateway's) and reads as a plausible
/// number rather than an error.
pub fn proc_match_problem(proc_match: &str) -> Option<String> {
    let m = proc_match.trim();
    if m.is_empty() {
        return Some("proc_match is empty, so nothing identifies the process".to_string());
    }
    if m.len() < MIN_PROC_MATCH_LEN {
        return Some(format!(
            "proc_match {m:?} is only {} characters; a substring that short matches command lines that have nothing to do with this gateway. Declare the binary path the gateway actually runs as (e.g. \"target/release/<binary>\")",
            m.len()
        ));
    }
    if GENERIC_PROC_MATCHES
        .iter()
        .any(|g| g.eq_ignore_ascii_case(m))
    {
        return Some(format!(
            "proc_match {m:?} is a generic command name: it matches processes that are not this gateway"
        ));
    }
    // The engine's own binary: a pattern in the harness's argv could match the harness itself, both
    // as a stop-signal target and as false proof-of-life for `is_alive`.
    if m.contains("otb") {
        return Some(format!(
            "proc_match {m:?} contains the engine's own binary name, so it can match the harness's command line rather than the gateway's"
        ));
    }
    None
}

/// Why a config setting exists. Every gateway config must be the bare minimum required to run, so
/// each setting must name which necessity it satisfies — an enum, so the build cannot express a
/// setting with no reason (or "we turned a feature on").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigReason {
    /// Needed to boot the gateway at all.
    RequiredToBoot,
    /// Points an upstream at the test mock instead of a real provider.
    UpstreamToMock,
    /// Exposes an ingress path the matrix exercises.
    ExposesIngress,
    /// Binds the port or the cores the rig requires.
    RigBinding,
}

/// One declared config setting, with the necessity that justifies it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSetting {
    pub key: String,
    pub reason: ConfigReason,
    /// Free text for a human, never load bearing.
    #[serde(default)]
    pub note: String,
}

// Same rule as `Runtime` above: a key the schema does not define is a defect, and one that
// silently parses is a defect nobody sees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Directory name. The gateway's identity on the board.
    pub name: String,
    pub display: String,
    pub lang: String,
    /// The project's OWN self-description, never our editorial.
    pub class: String,
    pub repo: String,
    pub port: u16,
    pub path: String,
    pub model: String,
    /// THE MODEL NAME THAT SELECTS EACH EGRESS COLUMN, keyed by egress dialect.
    ///
    /// Ingress is the wire shape (owned by `Dialect`); egress is the upstream, and most gateways
    /// choose it from the request's model name. A cell that doesn't vary the model doesn't vary the
    /// egress either — it sends the same request as the diagonal cell and gets mislabelled as a
    /// translation, collapsing six columns into one measurement published six times.
    ///
    /// The engine always asks for the egress's model (`model_for`); this map is just each gateway's
    /// own spelling of it, since some infer the provider from the name by their own convention.
    /// Absent for an egress means `model`.
    #[serde(default)]
    pub egress_models: std::collections::BTreeMap<String, String>,
    pub auth: String,
    #[serde(default)]
    pub headers: Vec<String>,
    pub runtime: Runtime,
    /// Egress dialects the manifest configures. NOT a capability claim: the matrix probes every cell
    /// regardless and publishes what it observes. This only says which upstreams are wired.
    #[serde(default)]
    pub egress: Vec<String>,
    /// THE DECLARED 6x6 CAPABILITY GRID: which (ingress, egress) pairings this gateway is expected
    /// to serve, researched against its own source/docs. Six rows (ingress) of six chars (egress),
    /// axis order `["openai", "openai-responses", "anthropic", "gemini", "cohere", "bedrock"]` both
    /// ways (`Dialect::all()` order). `'1'` = declared capable, `'0'` = declared not. Empty (default)
    /// means undeclared: every cell is probed.
    ///
    /// A `'0'` cell is NEVER PROBED, published `not_configurable` directly. A gateway's own global
    /// auth gate or rate limiter can answer an unsupported pairing with a real HTTP status (401,
    /// 429, ...) that fires before routing decides "no such route" — grading that as a probed
    /// failure would publish the gateway's front-door behaviour as a capability defect. A `'1'` cell,
    /// or any cell when this field is empty, is measured normally: observed status alone decides
    /// `NotConfigured` vs `Failed` (see `probe::persistent_transient_verdict`).
    #[serde(default)]
    pub matrix: Vec<String>,
    /// The cited, source-referenced reasoning behind `matrix`'s `'0'` cells, shown to a reader
    /// instead of a bare grey square.
    #[serde(default)]
    pub matrix_note: String,
    /// Cells the RIG cannot pose at all — distinct from a declared incapability: the gateway serves
    /// this pairing in production, but the harness's mock cannot stand in for the real upstream
    /// (e.g. a channel that signs requests to a fixed real hostname with no override).
    /// `"<ingress>/<egress>"` per entry. Never probed; published `untestable`, not graded either way.
    #[serde(default)]
    pub untestable: Vec<String>,
    #[serde(default)]
    pub untestable_note: String,
    /// A PER-CELL INGRESS PATH, for the cells that have one. Keyed `"<ingress>>egress"`.
    ///
    /// Some gateways route a dialect-matched request differently from a translated one; measuring
    /// the wrong route would measure work the gateway wouldn't actually do for an operator.
    ///
    /// Absent for a cell means the gateway's declared `path` (the dialect's standard path).
    ///
    /// Whatever route is used is RECORDED against the cell it produced: a provider-pinned route and
    /// the unified route are different measurements and must not be presented as the same one.
    #[serde(default)]
    pub cell_paths: std::collections::BTreeMap<String, String>,
    /// COMMANDS RUN AFTER THE GATEWAY IS UP, one per line, from a file named `commands`.
    ///
    /// Discovered by filename like `env` and `headers.json`, never declared, so every gateway is
    /// described the same way.
    ///
    /// For gateways with no config file at all — ones that store config in a database and are
    /// configured through their own admin API after boot. Almost every gateway needs none of this.
    ///
    /// Lines run in order, each through a shell, after the gateway is ready and before anything is
    /// measured. A line that fails fails the run: a half-configured gateway would otherwise answer
    /// probes and publish numbers for an upstream that was never wired up.
    #[serde(default, skip)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub config: Vec<ConfigSetting>,
    /// Headers that select the EGRESS, keyed by dialect.
    ///
    /// Some gateways route only by a request header, so for those the header IS the egress column.
    /// Without a per-column home, every column would be driven identically and the board would
    /// publish several identical columns as though they were different upstream dialects.
    ///
    /// Values admit the same placeholder set as everything else, since one gateway's routing header
    /// carries the rig-assigned mock port.
    #[serde(default)]
    pub egress_headers: std::collections::BTreeMap<String, Vec<String>>,
    /// Values this gateway's own templates refer to, beyond the closed set the harness supplies.
    ///
    /// A manifest declares a value like a model name or upstream URL ONCE here, and every template
    /// that needs it refers to it by name — a value spelled out separately in two places (a route
    /// URI and a probe path) can drift, which has previously cost measured cells.
    ///
    /// Values may themselves refer to the closed set, so `"url": "http://127.0.0.1:{MOCK_PORT}"`
    /// resolves at render time rather than being frozen at whatever port a previous run used.
    #[serde(default)]
    pub constants: std::collections::BTreeMap<String, String>,
    /// Config files the harness renders and the gateway reads.
    ///
    /// A TEMPLATE FILE in the gateway's own directory, not a string in this manifest and not Rust:
    /// `lib/gateway_isolation_test.sh` exempts `gateways/<name>/` from Rule 1 but scans `.rs`
    /// everywhere, so a per-gateway config (one requires the gateway's own name as a top-level key)
    /// has to live as a file beside the gateway, not generated by Rust code.
    #[serde(default)]
    pub config_files: Vec<ConfigFile>,
    /// How to START this gateway. `None` for a manifest that only describes a gateway someone else
    /// is running - which is every manifest today, because nothing in the tree could launch one.
    #[serde(default)]
    pub launch: Option<LaunchDecl>,
}

/// One config file: a template beside the gateway, and where the rendered result goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigFile {
    /// Template path, relative to the gateway's own directory.
    pub template: String,
    /// Rendered output, relative to the same directory. This is what a mount points at.
    pub output: String,
}

/// A file the gateway reads, rendered by the harness and handed to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountDecl {
    /// Path on the box, relative to the gateway's own directory.
    pub host_path: String,
    /// Where the gateway expects to find it.
    pub container_path: String,
    #[serde(default = "default_true")]
    pub read_only: bool,
}

fn default_true() -> bool {
    true
}

/// How many cores a cpuset string covers, e.g. "0-3" is four. A single core ("2") is one;
/// anything unparseable is one — claiming more parallelism than the pin allows is the direction
/// that corrupts a measurement.
/// Whether a path is a file this box can execute.
///
/// On a non-unix host the mode bits are unavailable, so existence is the best available answer;
/// the field runs on Linux where the distinction matters.
fn is_executable(p: &std::path::Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

fn core_count(cores: &str) -> u32 {
    let t = cores.trim();
    match t.split_once('-') {
        Some((lo, hi)) => match (lo.trim().parse::<u32>(), hi.trim().parse::<u32>()) {
            (Ok(lo), Ok(hi)) if hi >= lo => hi - lo + 1,
            _ => 1,
        },
        None => 1,
    }
}

/// Everything needed to start one gateway, as DATA.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LaunchDecl {
    /// An image, an env block, some mounts and a port. Ten of the thirteen entrants.
    Docker {
        image: String,
        /// Environment handed to the container, in declaration order.
        #[serde(default)]
        env: Vec<(String, String)>,
        /// Arguments after the image, for an entrypoint that takes them.
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        mounts: Vec<MountDecl>,
    },
    /// Built from source and run directly on the box.
    Native {
        /// A script in the gateway's own directory that produces the binary, run once before the
        /// first launch (never between egress columns). Installs a toolchain, so it must not run
        /// during the measurement window.
        #[serde(default)]
        build: Option<String>,
        /// Candidate paths to the built binary, first one that exists and is executable wins.
        ///
        /// A LIST because at least one entrant's crate emits no stable output name, requiring a
        /// search across candidates; declaring them keeps the search as visible data.
        binary: Vec<String>,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: Vec<(String, String)>,
        /// Names REMOVED from the child environment before it starts.
        ///
        /// NOT hygiene: at least one entrant's config loader rejects any unknown variable sharing
        /// its prefix, so the harness's own override variables would silently kill config load
        /// before the port binds (launch reports success; symptom is "port not listening").
        /// `std::process::Command` inherits the parent env and can only ADD, so removal needs its
        /// own field.
        #[serde(default)]
        env_unset: Vec<String>,
    },
}

/// Why a gateway could not be read from its directory. Every variant names the file: "manifest load
/// failed" with no path is the same as no message when a gateway has four of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestLoadError {
    Unreadable {
        path: std::path::PathBuf,
        why: String,
    },
    Malformed {
        path: std::path::PathBuf,
        why: String,
    },
}

impl std::fmt::Display for ManifestLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestLoadError::Unreadable { path, why } => {
                write!(f, "cannot read {}: {why}", path.display())
            }
            ManifestLoadError::Malformed { path, why } => {
                write!(f, "{} is not valid: {why}", path.display())
            }
        }
    }
}

impl std::error::Error for ManifestLoadError {}

/// Parse a sidecar env file: `KEY=value` sets, a leading `-` REMOVES.
///
/// Removal is not a convenience — see `LaunchDecl::Native::env_unset` for why an env block that
/// can only ADD can't express it.
///
/// Deliberately parsed, never executed: nothing in the measurement path runs untrusted config.
fn parse_env(raw: &str) -> (Vec<(String, String)>, Vec<String>) {
    let mut set = Vec::new();
    let mut unset = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('-') {
            unset.push(name.trim().to_string());
        } else if let Some((k, v)) = line.split_once('=') {
            set.push((k.trim().to_string(), v.trim().to_string()));
        }
    }
    (set, unset)
}

/// Why a config could not be rendered. Every variant names the file, because "config render failed"
/// with no path is the same as no message at all when a gateway has more than one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigRenderError {
    Unreadable {
        path: std::path::PathBuf,
        why: String,
    },
    Unwritable {
        path: std::path::PathBuf,
        why: String,
    },
    Placeholder {
        path: std::path::PathBuf,
        why: String,
    },
}

impl std::fmt::Display for ConfigRenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigRenderError::Unreadable { path, why } => {
                write!(f, "cannot read config template {}: {why}", path.display())
            }
            ConfigRenderError::Unwritable { path, why } => {
                write!(f, "cannot write rendered config {}: {why}", path.display())
            }
            ConfigRenderError::Placeholder { path, why } => {
                write!(f, "config template {}: {why}", path.display())
            }
        }
    }
}

impl std::error::Error for ConfigRenderError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    Empty(&'static str),
    BadPort,
    /// A field still carrying a shell variable that was never expanded (manifests extracted from
    /// shell can hold the reference, e.g. `$SOME_MODEL`, instead of the resolved value). Non-empty,
    /// so every other emptiness check passes it.
    UnexpandedVariable {
        field: &'static str,
        raw: String,
    },
    /// A config setting with no stated necessity cannot be lint-checked, so it cannot ship.
    ConfigWithoutReason(String),
    /// A launch declaration refers to something the harness does not supply. Loud rather than passed
    /// through: a `{TYPO}` reaching a container as a literal is a misconfiguration that boots and
    /// measures fine under the wrong settings.
    UnknownPlaceholder {
        name: String,
        raw: String,
    },
    /// A declared constant refers to itself, directly or through a ring of others.
    ConstantCycle {
        name: String,
    },
    /// A native `proc_match` too generic to name one process. Refused at load rather than at match
    /// time, since by match time the wrong process may already be signalled or measured.
    IndistinctProcMatch(String),
}

/// How deep a constant may refer to other constants before it is treated as a cycle. One real chain
/// exists (a path built from a model name); deeper is a mistake, not a design.
const MAX_CONSTANT_DEPTH: usize = 8;

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestError::Empty(field) => write!(f, "{field} must not be empty"),
            ManifestError::BadPort => write!(f, "port must be non-zero"),
            ManifestError::ConfigWithoutReason(k) => {
                write!(f, "config setting {k:?} has no key to attach a reason to")
            }
            ManifestError::IndistinctProcMatch(why) => write!(f, "{why}"),
            ManifestError::ConstantCycle { name } => {
                write!(f, "constant {name:?} refers to itself, directly or through a ring of others")
            }
            // Deliberately does NOT say where: raised for a launch declaration, config template, or
            // header value alike, and the caller already knows which. Naming the wrong one is worse
            // than naming none.
            ManifestError::UnknownPlaceholder { name, raw } => {
                let shown: String = raw.chars().take(60).collect();
                write!(
                    f,
                    "refers to {{{name}}}, which the harness does not supply. Use one of MOCK_PORT, GW_PORT, GW_MODEL, GW_AUTH, GW_DIR, CORES, NCORE, or declare it in `constants`. For a literal brace write {{{{ }}}}. In: {shown:?}"
                )
            }
            ManifestError::UnexpandedVariable { field, raw } => write!(
                f,
                "{field} still holds an unexpanded shell variable ({raw:?}): the extraction took the reference, not the value"
            ),
        }
    }
}

/// PARALLELISM, SET BY THE HARNESS FOR EVERY GATEWAY, FROM THE CORES IT PINNED.
///
/// A cpuset restricts which cores a process may run on but does not change what a runtime THINKS
/// is available — some read the machine's online CPU count and size thread pools for the whole box
/// while confined to a few cores, contending with itself and publishing a number nobody would
/// deploy. Set centrally, not left to each gateway's own env block, so a dropped setting can't
/// silently let a gateway boot unpinned.
///
/// Deliberately runtime-standard names only. A gateway with its own knob declares it in its own env
/// with `{NCORE}` instead, to keep per-gateway logic out of shared code.
fn pinned_parallelism(ncore: u32) -> Vec<(String, String)> {
    let n = ncore.to_string();
    [
        // Go honours affinity already; set anyway so the value is explicit everywhere.
        "GOMAXPROCS",
        // Node/libuv worker pool.
        "UV_THREADPOOL_SIZE",
        // Rust rayon.
        "RAYON_NUM_THREADS",
        // OpenMP, which numeric python stacks size their pools from.
        "OMP_NUM_THREADS",
    ]
    .iter()
    .map(|k| ((*k).to_string(), n.clone()))
    .collect()
}

/// This cell's declared capability, from a manifest's `matrix` (six rows of six `'0'`/`'1'` chars,
/// axis order `Dialect::ALL` both ways). `None` means undeclared (probe normally); `Some(false)`
/// must never be probed — see `Manifest::matrix`'s own doc for why.
pub fn matrix_declared_capable(matrix: &[String], ingress: &str, egress: &str) -> Option<bool> {
    let ing_i = crate::ingress::Dialect::ALL
        .iter()
        .position(|d| d.as_str() == ingress)?;
    let eg_i = crate::ingress::Dialect::ALL
        .iter()
        .position(|d| d.as_str() == egress)?;
    let row = matrix.get(ing_i)?;
    row.as_bytes().get(eg_i).map(|b| *b == b'1')
}

/// Whether this cell is one the RIG cannot pose at all (distinct from a declared incapability).
pub fn is_untestable_cell(untestable: &[String], ingress: &str, egress: &str) -> bool {
    untestable
        .iter()
        .any(|pair| pair.as_str() == format!("{ingress}/{egress}"))
}

impl Manifest {
    pub fn declared_capable(&self, ingress: &str, egress: &str) -> Option<bool> {
        matrix_declared_capable(&self.matrix, ingress, egress)
    }

    pub fn is_untestable_cell(&self, ingress: &str, egress: &str) -> bool {
        is_untestable_cell(&self.untestable, ingress, egress)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        for (v, field) in [
            (&self.name, "name"),
            (&self.display, "display"),
            (&self.repo, "repo"),
            (&self.path, "path"),
            (&self.model, "model"),
        ] {
            if v.trim().is_empty() {
                return Err(ManifestError::Empty(field));
            }
        }

        // A field holding an unexpanded shell reference is non-empty, so the emptiness check above
        // passes it and it survives to the wire — e.g. sent as the request's model, rejected by the
        // gateway, and misclassified by `probe.rs` as `NotConfigured`, publishing our own extraction
        // bug as the gateway's capability denial. No legitimate value of these fields contains `$`.
        for (v, field) in [
            (&self.name, "name"),
            (&self.display, "display"),
            (&self.repo, "repo"),
            (&self.path, "path"),
            (&self.model, "model"),
            (&self.auth, "auth"),
            (&self.lang, "lang"),
            (&self.class, "class"),
        ] {
            if v.contains('$') {
                return Err(ManifestError::UnexpandedVariable {
                    field,
                    raw: v.clone(),
                });
            }
        }
        for h in &self.headers {
            if h.contains('$') {
                return Err(ManifestError::UnexpandedVariable {
                    field: "headers",
                    raw: h.clone(),
                });
            }
        }
        if self.runtime.declared_identity().contains('$') {
            return Err(ManifestError::UnexpandedVariable {
                field: "runtime identity",
                raw: self.runtime.declared_identity().to_string(),
            });
        }
        if self.runtime.declared_identity().trim().is_empty() {
            return Err(ManifestError::Empty("runtime identity"));
        }
        // A NATIVE IDENTITY MUST BE DISTINCTIVE, checked here rather than trusted at match time: a
        // generic pattern selects a bystander process and publishes its memory (or kills it) under
        // this gateway's name.
        if let Runtime::Native { proc_match } = &self.runtime {
            if let Some(why) = proc_match_problem(proc_match) {
                return Err(ManifestError::IndistinctProcMatch(why));
            }
        }
        if self.port == 0 {
            return Err(ManifestError::BadPort);
        }
        for c in &self.config {
            if c.key.trim().is_empty() {
                return Err(ManifestError::ConfigWithoutReason(c.key.clone()));
            }
        }
        Ok(())
    }

    /// Values a launch declaration may refer to, resolved at launch time.
    ///
    /// A CLOSED SET, and small. `GOMAXPROCS={NCORE}` is why this exists at all: a literal there would
    /// run the gateway at the host's core count inside a smaller cpuset, corrupting the comparability
    /// basis of every number on the board. An unknown placeholder is an error rather than passed
    /// through, since a `{TYPO}` reaching a container as a literal boots and measures fine.
    fn substitute(
        &self,
        template: &str,
        cores: &str,
        mock_port: u16,
        gw_dir: &std::path::Path,
    ) -> Result<String, ManifestError> {
        self.substitute_at(template, cores, mock_port, gw_dir, 0)
    }

    fn substitute_at(
        &self,
        template: &str,
        cores: &str,
        mock_port: u16,
        gw_dir: &std::path::Path,
        depth: usize,
    ) -> Result<String, ManifestError> {
        let ncore = core_count(cores);
        let mut out = String::with_capacity(template.len());
        let mut rest = template;
        while let Some(at) = rest.find(['{', '}']) {
            out.push_str(&rest[..at]);
            let tail = &rest[at..];

            // A DOUBLED BRACE IS A LITERAL ONE, either way round. Config formats use braces of their
            // own (JSON, or a URL shape documented as `{api_base}`), so both halves must be
            // escapable: handling only `{{` would render `error_map: {}` as invalid `{}}`.
            if let Some(after) = tail.strip_prefix("{{") {
                out.push('{');
                rest = after;
                continue;
            }
            if let Some(after) = tail.strip_prefix("}}") {
                out.push('}');
                rest = after;
                continue;
            }
            // A lone closing brace is content: nothing opened it.
            if let Some(after) = tail.strip_prefix('}') {
                out.push('}');
                rest = after;
                continue;
            }

            let Some(close) = tail.find('}') else {
                // An unmatched opening brace is content too, not a truncated placeholder.
                out.push_str(tail);
                return Ok(out);
            };
            let name = &tail[1..close];
            let value = match name {
                "NCORE" => ncore.to_string(),
                "CORES" => cores.to_string(),
                "GW_PORT" => self.port.to_string(),
                "MOCK_PORT" => mock_port.to_string(),
                "GW_AUTH" => self.auth.clone(),
                "GW_DIR" => gw_dir.to_string_lossy().into_owned(),
                "GW_MODEL" => self.model.clone(),
                // A manifest's own declared constant, resolved recursively (e.g. a bedrock path
                // built from a bedrock model constant). The depth bound stops a self- or ring-
                // referential constant with a named error instead of a stack overflow.
                name if self.constants.contains_key(name) => {
                    if depth >= MAX_CONSTANT_DEPTH {
                        return Err(ManifestError::ConstantCycle {
                            name: name.to_string(),
                        });
                    }
                    let raw = self.constants.get(name).cloned().unwrap_or_default();
                    self.substitute_at(&raw, cores, mock_port, gw_dir, depth + 1)?
                }
                other => {
                    return Err(ManifestError::UnknownPlaceholder {
                        name: other.to_string(),
                        raw: template.to_string(),
                    })
                }
            };
            out.push_str(&value);
            rest = &tail[close + 1..];
        }
        out.push_str(rest);
        Ok(out)
    }

    /// The launch this manifest describes, ready to hand to `launch::launch`.
    ///
    /// `None` when the manifest declares no launch: the harness is then driving a gateway someone
    /// else started.
    ///
    /// The container's `--name` is NOT taken from here — it comes from `runtime.identity()`, the
    /// same string the memory readers and the stop path use, so the started, measured, and stopped
    /// container can never diverge.
    ///
    /// `gw_dir` resolves the mounts: config files are declared relative to the gateway's own
    /// directory, since an absolute path in a manifest only works on one machine.
    pub fn launch_spec(
        &self,
        cores: &str,
        mock_port: u16,
        gw_dir: &std::path::Path,
        ready_budget: std::time::Duration,
        boot_backoff: std::time::Duration,
    ) -> Option<Result<crate::launch::LaunchSpec, ManifestError>> {
        let decl = self.launch.as_ref()?;
        let subst = |v: &str| self.substitute(v, cores, mock_port, gw_dir);
        let subst_all = |xs: &[String]| -> Result<Vec<String>, ManifestError> {
            xs.iter().map(|x| subst(x)).collect()
        };
        let subst_env = |xs: &[(String, String)]| -> Result<Vec<(String, String)>, ManifestError> {
            xs.iter().map(|(k, v)| Ok((k.clone(), subst(v)?))).collect()
        };

        // A declared `build` script must actually run before launch, or a source-built entrant's
        // launcher is told to run a binary nothing produced and never becomes ready. Run ONCE,
        // before the first attempt: it installs a toolchain and compiles, which must not happen
        // inside a measurement window.
        let pre_launch = match decl {
            LaunchDecl::Native {
                build: Some(script),
                ..
            } => Some(crate::launch::PreLaunchStep {
                command: gw_dir.join(script).to_string_lossy().into_owned(),
                args: Vec::new(),
                // Generous on purpose: this bound exists to catch a hang, not a merely slow build.
                timeout: std::time::Duration::from_secs(30 * 60),
            }),
            _ => None,
        };

        let kind = match decl {
            LaunchDecl::Docker {
                image,
                env,
                args,
                mounts,
            } => {
                let env = match subst_env(env) {
                    Ok(e) => e,
                    Err(e) => return Some(Err(e)),
                };
                let args = match subst_all(args) {
                    Ok(a) => a,
                    Err(e) => return Some(Err(e)),
                };
                crate::launch::LaunchKind::Docker {
                    image: image.clone(),
                    // THE HARNESS WINS: pinning values go last, since a later assignment overrides
                    // an earlier one and no entrant may opt out of its core limit.
                    env: env
                        .into_iter()
                        .chain(pinned_parallelism(core_count(cores)))
                        .collect(),
                    // Host networking: a published port mapping would put a NAT hop inside every
                    // measured request.
                    port: crate::launch::PortMapping::Host,
                    mounts: mounts
                        .iter()
                        .map(|m| crate::launch::Mount {
                            // ABSOLUTE: a container runtime reads a relative source as a named
                            // VOLUME, not a path, and refuses it. Canonicalize where possible; fall
                            // back to joining if the file doesn't exist yet, so a missing config
                            // fails the launch rather than silently producing a relative path.
                            host_path: {
                                let p = gw_dir.join(&m.host_path);
                                std::fs::canonicalize(&p)
                                    .unwrap_or_else(|_| {
                                        std::env::current_dir().map(|c| c.join(&p)).unwrap_or(p)
                                    })
                                    .to_string_lossy()
                                    .into_owned()
                            },
                            container_path: m.container_path.clone(),
                            read_only: m.read_only,
                        })
                        .collect(),
                    command: args,
                }
            }
            LaunchDecl::Native {
                build,
                binary,
                args,
                env,
                env_unset,
            } => {
                // The FIRST declared candidate that exists and is executable. Falls back to the
                // first candidate when none exists yet (before a build runs): `launch` must then
                // fail with its own evidence rather than this looking like "no launch declared".
                let resolved = binary
                    .iter()
                    .map(|b| gw_dir.join(b))
                    .find(|p| is_executable(p))
                    .or_else(|| binary.first().map(|b| gw_dir.join(b)));
                let Some(bin) = resolved else {
                    return Some(Err(ManifestError::Empty("launch binary")));
                };
                let env = match subst_env(env) {
                    Ok(e) => e,
                    Err(e) => return Some(Err(e)),
                };
                let args = match subst_all(args) {
                    Ok(a) => a,
                    Err(e) => return Some(Err(e)),
                };
                let _ = build; // consumed by the pre-launch step above
                crate::launch::LaunchKind::Native {
                    binary: bin.to_string_lossy().into_owned(),
                    args,
                    // Same precedence as the container path: the harness states the core limit last.
                    env: env
                        .into_iter()
                        .chain(pinned_parallelism(core_count(cores)))
                        .collect(),
                    env_unset: env_unset.clone(),
                }
            }
        };

        Some(Ok(crate::launch::LaunchSpec {
            runtime: self.runtime.clone(),
            kind,
            cores: cores.to_string(),
            port: self.port,
            ready_budget,
            boot_backoff,
            pre_launch,
        }))
    }

    /// Everything wrong with this gateway's setup, in one pass.
    ///
    /// ALL of it, not the first thing found — a game of fix-one-rerun is worse for someone adding a
    /// gateway. Every finding names the file and says what to do. These are the mistakes that
    /// otherwise surface as a container that starts and immediately dies, reading as the gateway
    /// being broken rather than the setup being wrong.
    pub fn problems(&self, gw_dir: &std::path::Path) -> Vec<String> {
        let mut out = Vec::new();

        if let Err(e) = self.validate() {
            out.push(format!("definition.json: {e}"));
        }

        // A HEADER LINE WITH NO COLON IS SILENTLY NEVER SENT: `headers_for` parses each line with
        // `split_once(':')` and no else arm, so "x-api-key bench-token" (missing its colon) is
        // dropped without a word, and the gateway measures without a header it declared — often
        // indistinguishable from a 401 on every probe. Caught here, at validate time, before the
        // box-hours are spent.
        // A CELL PATH THAT CANNOT SUBSTITUTE MEASURES THE WRONG ROUTE: `cell_paths_for` drops it and
        // falls back to the dialect's default path, publishing a different wire under the cell's
        // name. Caught here for the same reason.
        for (k, v) in &self.cell_paths {
            if let Err(e) = self.substitute(v, "0-3", 8000, gw_dir) {
                out.push(format!(
                    "cell_paths[{k}]: {v:?} cannot be substituted ({e}), so this cell would measure \
                     the dialect's default route instead of the declared one"
                ));
            }
        }

        for (label, line) in
            self.headers
                .iter()
                .map(|l| ("headers".to_string(), l))
                .chain(self.egress_headers.iter().flat_map(|(eg, ls)| {
                    ls.iter().map(move |l| (format!("egress_headers[{eg}]"), l))
                }))
        {
            if !line.contains(':') {
                out.push(format!(
                    "{label}: header {line:?} has no colon, so it would be dropped and never sent - \
                     write it as it appears on the wire, \"Name: value\""
                ));
            }
        }

        // A build script declared but not there would point the launcher at a binary nothing built,
        // surfacing on a bench box as `never became ready` instead of here, in a second.
        if let Some(crate::manifest::LaunchDecl::Native {
            build: Some(script),
            ..
        }) = &self.launch
        {
            let path = gw_dir.join(script);
            if !path.is_file() {
                out.push(format!(
                    "definition.json declares build script {script:?}, but {} does not exist, so the binary it is supposed to produce never will either",
                    path.display()
                ));
            } else if !is_executable(&path) {
                out.push(format!(
                    "build script {} is not executable, so the launcher cannot run it",
                    path.display()
                ));
            }
        }

        // A template that is declared but not there.
        for f in &self.config_files {
            let t = gw_dir.join(&f.template);
            if !t.is_file() {
                out.push(format!(
                    "definition.json declares config template {:?}, but {} does not exist",
                    f.template,
                    t.display()
                ));
                continue;
            }
            // A CONFIG TEMPLATE IS DATA, AND MUST NOT ASK FOR A COMMAND TO BE RUN: the engine only
            // substitutes `{PLACEHOLDER}`, so a leftover shell `$(...)` (from the retired shell
            // manifests) writes out verbatim — the gateway then boots, binds its port, answers every
            // probe 404, and publishes as serving nothing at all.
            //
            // A TEMPLATE WE COULD NOT READ IS NOT A TEMPLATE THAT PASSED: without an else arm here, a
            // template that exists but can't be read (permissions, transient EIO, NFS hiccup) would
            // skip both checks below and report nothing wrong, indistinguishable from "checked and
            // clean".
            let raw = match std::fs::read_to_string(&t) {
                Ok(raw) => Some(raw),
                Err(e) => {
                    out.push(format!(
                        "{}: exists but could not be read ({e}), so neither the shell-substitution \
                         nor the placeholder check could run on it. An unchecked template is not a \
                         clean one",
                        f.template
                    ));
                    None
                }
            };
            if let Some(raw) = raw {
                if let Some(bad) = raw.lines().find(|l| l.contains("$(")) {
                    out.push(format!(
                        "{}: contains a shell command substitution, which nothing will expand: {:?}. A config template is data; write the literal value or use a {{PLACEHOLDER}}",
                        f.template,
                        bad.trim()
                    ));
                }
                if let Err(e) = self.substitute(&raw, "0-3", 8000, gw_dir) {
                    out.push(format!("{}: {e}", f.template));
                }
            }
        }

        // A mount pointing at a file nothing renders costs the most to diagnose in the wild: the
        // container starts, finds no config, and exits, with only a port that never listens.
        if let Some(LaunchDecl::Docker { mounts, .. }) = &self.launch {
            for m in mounts {
                let rendered = self.config_files.iter().any(|f| f.output == m.host_path);
                let on_disk = gw_dir.join(&m.host_path).exists();
                if !rendered && !on_disk {
                    out.push(format!(
                        "launch mounts {:?}, but no config_files entry renders it and no such file exists. Either add a config_files entry whose output is {:?}, or check the path",
                        m.host_path, m.host_path
                    ));
                }
            }
        }

        // A native entrant with no binary to run.
        if let Some(LaunchDecl::Native { binary, .. }) = &self.launch {
            if binary.is_empty() {
                out.push(
                    "launch declares no binary candidates, so there is nothing to start"
                        .to_string(),
                );
            }
        }

        // An egress column declaring headers that the run will never walk.
        for column in self.egress_headers.keys() {
            if column.parse::<crate::ingress::Dialect>().is_err() {
                out.push(format!(
                    "headers.json has an entry for {column:?}, which is not a dialect this benchmark speaks"
                ));
            }
        }

        // The config-necessity standard, reported alongside everything else rather than as a
        // separate tool.
        for f in crate::config_lint::lint(self, &Default::default()) {
            out.push(format!("definition.json: {}", f.message));
        }

        out
    }

    /// Read a gateway from its own directory: the definition, plus whatever sidecars it has.
    ///
    /// ONE FILE IS UNIFORM AND THE REST ARE THE GATEWAY'S OWN. `definition.json` has the same shape
    /// for every entrant; everything that differs (env, upstream-selecting headers, boot config)
    /// sits beside it in whatever form that thing naturally takes.
    ///
    /// Every sidecar is optional; some entrants need none at all.
    pub fn load(dir: &std::path::Path) -> Result<Manifest, ManifestLoadError> {
        let def_path = dir.join("definition.json");
        let text =
            std::fs::read_to_string(&def_path).map_err(|e| ManifestLoadError::Unreadable {
                path: def_path.clone(),
                why: e.to_string(),
            })?;
        let mut m: Manifest =
            serde_json::from_str(&text).map_err(|e| ManifestLoadError::Malformed {
                path: def_path.clone(),
                why: e.to_string(),
            })?;

        let env_path = dir.join("env");
        if env_path.is_file() {
            let raw =
                std::fs::read_to_string(&env_path).map_err(|e| ManifestLoadError::Unreadable {
                    path: env_path.clone(),
                    why: e.to_string(),
                })?;
            let (env, unset) = parse_env(&raw);
            m.apply_env(env, unset);
        }

        // One command per line; blank lines and `#` comments are skipped so the file can explain
        // itself.
        let commands_path = dir.join("commands");
        if commands_path.is_file() {
            let raw = std::fs::read_to_string(&commands_path).map_err(|e| {
                ManifestLoadError::Unreadable {
                    path: commands_path.clone(),
                    why: e.to_string(),
                }
            })?;
            m.commands = raw
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_string)
                .collect();
        }

        let headers_path = dir.join("headers.json");
        if headers_path.is_file() {
            let raw = std::fs::read_to_string(&headers_path).map_err(|e| {
                ManifestLoadError::Unreadable {
                    path: headers_path.clone(),
                    why: e.to_string(),
                }
            })?;
            m.egress_headers =
                serde_json::from_str(&raw).map_err(|e| ManifestLoadError::Malformed {
                    path: headers_path.clone(),
                    why: e.to_string(),
                })?;
        }
        Ok(m)
    }

    /// Manifest headers that name something the RIG already sends on every request of some dialect.
    ///
    /// `run::headers_for` composes the dialect's own credential header (`Dialect::auth_headers`)
    /// then appends the manifest's `headers`/`egress_headers` verbatim. Nothing stops a manifest
    /// also declaring e.g. `authorization` itself (one real manifest does), colliding with the
    /// header a dialect sends — HTTP does not define which of two same-named headers a server
    /// honours, so the gateway authenticates as SOMEBODY and still publishes a clean-looking number.
    ///
    /// PRECEDENCE, NOT REFUSAL: refusing at load would stop the whole benchmark on a first-party file
    /// this change may not edit, trading an ambiguous measurement for no measurement. Instead the
    /// wire is made unambiguous (`run::headers_for` drops the manifest's copy, keeps the dialect's)
    /// and the collision is DISCLOSED here so `otb validate` names it.
    ///
    /// The rig's copy wins because the credential is the harness's to assert (`cfg.auth`); a
    /// manifest that could override it could measure the gateway under an identity the harness
    /// cannot name.
    ///
    /// The name list is DERIVED from `Dialect::auth_headers` via `rig_owned_header_names`, over every
    /// dialect (not just the ones this gateway wires up), since the matrix probes all six regardless
    /// and a header in `headers` is sent on every one of them.
    pub fn rig_owned_headers_declared(&self) -> Vec<String> {
        let owned: std::collections::BTreeSet<String> = crate::ingress::Dialect::ALL
            .iter()
            .flat_map(|d| d.rig_owned_header_names())
            .collect();
        let declared = self.headers.iter().map(|l| ("headers", l)).chain(
            self.egress_headers
                .iter()
                .flat_map(|(col, lines)| lines.iter().map(move |l| (col.as_str(), l))),
        );
        let mut out = Vec::new();
        for (where_, line) in declared {
            let Some((name, _)) = line.split_once(':') else {
                continue;
            };
            let name = name.trim().to_ascii_lowercase();
            if owned.contains(&name) {
                out.push(format!(
                    "declares the header {name:?} (in {where_}), which the harness already sends itself \
                     on every request of the dialects that use it. Only the harness's own copy goes on \
                     the wire (`run::headers_for` drops this one), because two same-named headers on one \
                     request is a credential or route no reader of the board could name. Remove it from \
                     the manifest"
                ));
            }
        }
        out
    }

    /// Put a sidecar's env onto whichever launch kind this manifest declares.
    fn apply_env(&mut self, env: Vec<(String, String)>, unset: Vec<String>) {
        match self.launch.as_mut() {
            Some(LaunchDecl::Docker { env: e, .. }) => *e = env,
            Some(LaunchDecl::Native {
                env: e,
                env_unset: u,
                ..
            }) => {
                *e = env;
                *u = unset;
            }
            None => {}
        }
    }

    /// `cell_paths` with its placeholders resolved, the same way `headers_for` resolves theirs.
    ///
    /// A per-cell path is where a placeholder earns its keep: e.g. Bedrock's standard path embeds
    /// the model, built from a constant the manifest already declares. Sending the raw `{NAME}`
    /// would get a 404, published as the gateway not serving a pairing it does — an unearned red
    /// caused by us.
    ///
    /// Resolving here rather than pinning the expanded string in `cell_paths` keeps ONE source for
    /// the value, so it can't go stale the day the constant changes.
    ///
    /// A key that cannot be resolved is dropped rather than sent half-substituted, so `path_for`
    /// falls back to the dialect's standard path — wrong but honest, never our own template syntax.
    pub fn cell_paths_for(
        &self,
        cores: &str,
        mock_port: u16,
        gw_dir: &std::path::Path,
    ) -> std::collections::BTreeMap<String, String> {
        self.cell_paths
            .iter()
            .filter_map(|(k, v)| {
                match self.substitute(v, cores, mock_port, gw_dir) {
                    Ok(path) => Some((k.clone(), path)),
                    // A silently-dropped cell_path would leave no entry and no word, and the cell
                    // would measure the DIALECT'S DEFAULT path instead — a different route than
                    // declared, published under the cell's name. Logged instead.
                    Err(e) => {
                        eprintln!(
                            "manifest: cell path for {k:?} could not be substituted ({e}), so this \
                             cell would fall back to the dialect's default route rather than the one \
                             its manifest declares"
                        );
                        None
                    }
                }
            })
            .collect()
    }

    /// The headers to send for one egress column: the manifest's always-on headers, then the ones
    /// that select this column.
    ///
    /// `authorization` is added by the caller, not here: it's the same for every column and minted
    /// at launch rather than declared.
    pub fn headers_for(
        &self,
        egress: &str,
        cores: &str,
        mock_port: u16,
        gw_dir: &std::path::Path,
    ) -> Result<Vec<(String, String)>, ManifestError> {
        let mut out = Vec::new();
        let lines = self
            .headers
            .iter()
            .chain(self.egress_headers.get(egress).into_iter().flatten());
        for line in lines {
            let resolved = self.substitute(line, cores, mock_port, gw_dir)?;
            // Written the way they appear on the wire: "Name: value".
            if let Some((name, value)) = resolved.split_once(':') {
                out.push((name.trim().to_string(), value.trim().to_string()));
            }
        }
        Ok(out)
    }

    /// Render every declared config file into the gateway's directory.
    ///
    /// Returns the paths written, so a caller can publish them as the artifact's config record — the
    /// bytes a gateway booted with belong beside the numbers they produced, so a chart can't later be
    /// read against a config that was overwritten.
    ///
    /// A template referring to something the harness does not supply is an ERROR, not a passthrough:
    /// a gateway booting with a literal `{MOCK_PORT}` fails in a way that looks like the gateway
    /// being broken.
    pub fn render_configs(
        &self,
        cores: &str,
        mock_port: u16,
        gw_dir: &std::path::Path,
    ) -> Result<Vec<(std::path::PathBuf, String)>, ConfigRenderError> {
        let mut written = Vec::new();
        for file in &self.config_files {
            let template_path = gw_dir.join(&file.template);
            let raw = std::fs::read_to_string(&template_path).map_err(|e| {
                ConfigRenderError::Unreadable {
                    path: template_path.clone(),
                    why: e.to_string(),
                }
            })?;
            let body = self
                .substitute(&raw, cores, mock_port, gw_dir)
                .map_err(|e| ConfigRenderError::Placeholder {
                    path: template_path.clone(),
                    why: e.to_string(),
                })?;
            let out_path = gw_dir.join(&file.output);
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| ConfigRenderError::Unwritable {
                    path: out_path.clone(),
                    why: e.to_string(),
                })?;
            }
            std::fs::write(&out_path, &body).map_err(|e| ConfigRenderError::Unwritable {
                path: out_path.clone(),
                why: e.to_string(),
            })?;
            written.push((out_path, body));
        }
        Ok(written)
    }

    /// The URL the harness drives this gateway on.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}{}", self.port, self.path)
    }
}

/// A minimal, valid `Manifest` for tests across this crate. Callers override only the fields their
/// test cares about with struct-update syntax (`Manifest { egress: vec![...], ..test_fixture() }`),
/// so a new field is one edit here, not one per call site.
#[cfg(test)]
pub(crate) fn test_fixture() -> Manifest {
    Manifest {
        name: "gw".into(),
        display: "GW".into(),
        lang: "Rust".into(),
        class: "AI gateway".into(),
        repo: "https://example.invalid/gw".into(),
        port: 8080,
        path: "/v1/chat/completions".into(),
        model: "m".into(),
        egress_models: Default::default(),
        auth: "dummy".into(),
        headers: vec![],
        runtime: Runtime::Docker {
            container: "gw-bench".into(),
            run_scope: None,
        },
        egress: vec!["openai".into()],
        matrix: vec![],
        matrix_note: String::new(),
        untestable: vec![],
        untestable_note: String::new(),
        commands: vec![],
        cell_paths: Default::default(),
        config: vec![],
        launch: None,
        config_files: vec![],
        constants: Default::default(),
        egress_headers: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    // A header line missing its colon is dropped without a word by `headers_for`, and the gateway
    // measures without a header it declared — indistinguishable from one genuinely not serving.
    #[test]
    fn a_header_line_without_a_colon_is_a_validate_problem() {
        let dir = std::env::temp_dir();
        let mut m = test_fixture();
        m.headers = vec!["x-api-key bench-token".to_string()];
        let found = m.problems(&dir);
        assert!(
            found
                .iter()
                .any(|p| p.contains("no colon") && p.contains("x-api-key")),
            "a colonless header must be reported, got: {found:?}"
        );

        // The wire-shaped form is fine, and so is a value that itself contains a colon (a URL).
        m.headers = vec![
            "x-api-key: bench-token".to_string(),
            "x-base-url: http://127.0.0.1:8000".to_string(),
        ];
        let found = m.problems(&dir);
        assert!(
            !found.iter().any(|p| p.contains("no colon")),
            "a properly formed header must not be reported, got: {found:?}"
        );
    }

    use super::*;
    use std::time::Duration;

    fn docker_manifest() -> Manifest {
        test_fixture()
    }

    // Prevents an unearned red: an unresolved `{NAME}` reaching the probe as a literal gets a
    // truthful 404, publishing a declared-capable pairing as unserved. Nesting is exercised because
    // the real case is a path constant referring to a model constant.
    #[test]
    fn a_cell_path_resolves_its_placeholders_before_it_ever_reaches_a_probe() {
        let mut m = docker_manifest();
        m.constants
            .insert("BEDROCK_MODEL".into(), "vendor.model-v1:0".into());
        m.constants.insert(
            "MATRIX_PATH_BEDROCK".into(),
            "/model/{BEDROCK_MODEL}/converse".into(),
        );
        m.cell_paths
            .insert("bedrock>bedrock".into(), "{MATRIX_PATH_BEDROCK}".into());

        let got = m.cell_paths_for("0-3", 8000, std::path::Path::new("."));
        assert_eq!(
            got.get("bedrock>bedrock").map(String::as_str),
            Some("/model/vendor.model-v1:0/converse"),
            "a nested constant must be fully resolved, never passed through as template syntax"
        );
    }

    // An unresolvable key is DROPPED, so `path_for` falls back to the dialect's standard path: a
    // wrong-but-honest probe of a real endpoint beats asking a gateway for `{TYPO}`.
    #[test]
    fn an_unresolvable_cell_path_is_dropped_rather_than_sent_half_substituted() {
        let mut m = docker_manifest();
        m.cell_paths.insert(
            "bedrock>bedrock".into(),
            "/model/{NO_SUCH_CONSTANT}/converse".into(),
        );
        assert!(m
            .cell_paths_for("0-3", 8000, std::path::Path::new("."))
            .is_empty());
    }

    // Axis order is `Dialect::ALL` both ways: row 0 = openai ingress, col 0 = openai egress.
    #[test]
    fn matrix_reads_row_ingress_col_egress_in_dialect_order() {
        let matrix = vec![
            "100000".to_string(), // openai ingress: only openai egress capable
            "000000".to_string(),
            "001000".to_string(), // anthropic ingress: only anthropic egress capable
            "000000".to_string(),
            "000000".to_string(),
            "000000".to_string(),
        ];
        assert_eq!(
            matrix_declared_capable(&matrix, "openai", "openai"),
            Some(true)
        );
        assert_eq!(
            matrix_declared_capable(&matrix, "openai", "anthropic"),
            Some(false)
        );
        assert_eq!(
            matrix_declared_capable(&matrix, "anthropic", "anthropic"),
            Some(true)
        );
        assert_eq!(
            matrix_declared_capable(&matrix, "gemini", "openai"),
            Some(false)
        );
    }

    // Empty matrix means undeclared: every cell probes normally, unchanged from before this field
    // existed - a gateway that has not been researched yet must not be silently treated as
    // incapable of everything.
    #[test]
    fn an_empty_matrix_means_undeclared_not_incapable() {
        assert_eq!(matrix_declared_capable(&[], "openai", "openai"), None);
    }

    #[test]
    fn a_dialect_name_the_matrix_does_not_recognise_is_also_undeclared() {
        let matrix = vec!["1".repeat(6); 6];
        assert_eq!(
            matrix_declared_capable(&matrix, "not-a-real-dialect", "openai"),
            None
        );
    }

    #[test]
    fn untestable_cells_match_the_exact_ingress_egress_pair_only() {
        let untestable = vec!["openai/bedrock".to_string()];
        assert!(is_untestable_cell(&untestable, "openai", "bedrock"));
        assert!(
            !is_untestable_cell(&untestable, "bedrock", "openai"),
            "direction matters"
        );
        assert!(!is_untestable_cell(&untestable, "openai", "anthropic"));
    }

    // RSS, HWM and stop all read ONE declaration, so they cannot name different things.
    #[test]
    fn every_reader_derives_from_one_identity() {
        let m = docker_manifest();
        let id = m.runtime.identity();
        assert_eq!(id, "gw-bench");
        // There is no second place to spell it, so there is nothing for a reader to disagree with.
        assert_eq!(m.runtime.identity(), id);
    }

    #[test]
    fn a_native_runtime_carries_a_process_match_not_a_container() {
        let m = Manifest {
            runtime: Runtime::Native {
                proc_match: "target/release/gw".into(),
            },
            ..docker_manifest()
        };
        assert!(!m.runtime.is_docker());
        assert_eq!(m.runtime.identity(), "target/release/gw");
    }

    // ---- run-scoped container identity ------------------------------------------------------------

    // Without scoping, `--name` would be identical on every run of this gateway on this box, and a
    // second run's boot-retry `docker rm -f` could delete the first run's container mid-measurement.
    #[test]
    fn two_runs_of_one_gateway_cannot_name_the_same_container() {
        let declared = Runtime::Docker {
            container: "gw-bench".into(),
            run_scope: None,
        };
        let a = declared.scoped_to_run("20260729-101500-4242");
        let b = declared.scoped_to_run("20260729-101500-4243");
        assert_ne!(
            a.identity(),
            b.identity(),
            "two concurrent runs must not name the same container"
        );
        assert!(a.identity().starts_with("gw-bench-"), "{}", a.identity());
        // The declared name stays available for the label a cross-run sweep filters on.
        assert_eq!(a.declared_identity(), "gw-bench");
        assert_eq!(a.run_scope(), Some("20260729-101500-4242"));
    }

    // `proc_match` matches a command line the gateway itself produces; scoping it would match
    // nothing.
    #[test]
    fn scoping_a_native_identity_leaves_the_process_match_alone() {
        let rt = Runtime::Native {
            proc_match: "target/release/gw".into(),
        };
        assert_eq!(rt.scoped_to_run("run-1").identity(), "target/release/gw");
        assert_eq!(rt.scoped_to_run("run-1").run_scope(), None);
    }

    // A run id a container runtime would reject must not produce a name that cannot be launched.
    #[test]
    fn a_run_id_is_reduced_to_what_a_container_name_accepts() {
        let declared = Runtime::Docker {
            container: "gw".into(),
            run_scope: None,
        };
        assert_eq!(declared.scoped_to_run("a b/c:d").identity(), "gw-abcd");
        assert_eq!(
            declared.scoped_to_run("///").identity(),
            "gw",
            "a run id that survives as nothing leaves the name unscoped, never trailing a separator"
        );
    }

    // ---- a proc_match must name one process --------------------------------------------------------

    // Matching is a SUBSTRING against every command line on the box, so a short or generic pattern
    // selects a crowd.
    #[test]
    fn an_indistinct_proc_match_is_refused_at_load_not_discovered_at_match_time() {
        for bad in ["gw", "node", "server", "sh", "otb-run"] {
            let m = Manifest {
                runtime: Runtime::Native {
                    proc_match: bad.into(),
                },
                ..docker_manifest()
            };
            assert!(
                matches!(m.validate(), Err(ManifestError::IndistinctProcMatch(_))),
                "{bad:?} must not validate: it matches command lines that are not this gateway"
            );
        }
    }

    #[test]
    fn a_binary_path_is_a_distinctive_enough_proc_match() {
        for good in [
            "target/release/aisix",
            "target/release/ai-gateway",
            "litellm-ai-gateway",
        ] {
            assert_eq!(proc_match_problem(good), None, "{good:?} was refused");
        }
    }

    // A runtime with no identity cannot be measured or stopped, so it must not validate.
    #[test]
    fn an_empty_runtime_identity_is_rejected() {
        let m = Manifest {
            runtime: Runtime::Docker {
                container: "  ".into(),
                run_scope: None,
            },
            ..docker_manifest()
        };
        assert_eq!(m.validate(), Err(ManifestError::Empty("runtime identity")));
    }

    #[test]
    fn required_fields_are_required() {
        for (mutate, field) in [
            (
                Box::new(|m: &mut Manifest| m.name.clear()) as Box<dyn Fn(&mut Manifest)>,
                "name",
            ),
            (Box::new(|m: &mut Manifest| m.display.clear()), "display"),
            (Box::new(|m: &mut Manifest| m.repo.clear()), "repo"),
            (Box::new(|m: &mut Manifest| m.path.clear()), "path"),
            (Box::new(|m: &mut Manifest| m.model.clear()), "model"),
        ] {
            let mut m = docker_manifest();
            mutate(&mut m);
            assert_eq!(
                m.validate(),
                Err(ManifestError::Empty(field)),
                "{field} must be required"
            );
        }
        let mut m = docker_manifest();
        m.port = 0;
        assert_eq!(m.validate(), Err(ManifestError::BadPort));
    }

    // A setting cannot be constructed without naming which necessity it satisfies.
    #[test]
    fn a_config_setting_must_name_its_necessity() {
        let m = Manifest {
            config: vec![ConfigSetting {
                key: "listen.port".into(),
                reason: ConfigReason::RigBinding,
                note: "the rig pins the port".into(),
            }],
            ..docker_manifest()
        };
        assert!(m.validate().is_ok());
        assert_eq!(m.config[0].reason, ConfigReason::RigBinding);
    }

    // Extracted-from-shell manifests can hold the reference (`$SOME_MODEL`) instead of the value;
    // that field is non-empty and passes every other rule, then goes out on the wire as a model name.
    #[test]
    fn a_field_holding_an_unexpanded_shell_variable_is_rejected() {
        let m = Manifest {
            model: "$SOME_MODEL".into(),
            ..docker_manifest()
        };
        assert_eq!(
            m.validate(),
            Err(ManifestError::UnexpandedVariable {
                field: "model",
                raw: "$SOME_MODEL".into()
            })
        );
        assert!(
            !m.model.trim().is_empty(),
            "the point: it is non-empty, so the emptiness checks pass it"
        );

        // Every field a request or a launch is built from, not just the model.
        let m = Manifest {
            auth: "${GW_KEY}".into(),
            ..docker_manifest()
        };
        assert!(matches!(
            m.validate(),
            Err(ManifestError::UnexpandedVariable { field: "auth", .. })
        ));
        let m = Manifest {
            headers: vec!["x-api-key: $GW_AUTH".into()],
            ..docker_manifest()
        };
        assert!(matches!(
            m.validate(),
            Err(ManifestError::UnexpandedVariable {
                field: "headers",
                ..
            })
        ));
        let m = Manifest {
            runtime: Runtime::Docker {
                container: "$NAME-bench".into(),
                run_scope: None,
            },
            ..docker_manifest()
        };
        assert!(matches!(
            m.validate(),
            Err(ManifestError::UnexpandedVariable {
                field: "runtime identity",
                ..
            })
        ));

        // A clean manifest is untouched by the new rule.
        assert!(docker_manifest().validate().is_ok());
    }

    #[test]
    fn a_setting_with_no_key_cannot_ship() {
        let m = Manifest {
            config: vec![ConfigSetting {
                key: " ".into(),
                reason: ConfigReason::RequiredToBoot,
                note: String::new(),
            }],
            ..docker_manifest()
        };
        assert!(matches!(
            m.validate(),
            Err(ManifestError::ConfigWithoutReason(_))
        ));
    }

    #[test]
    fn round_trips_through_json_including_the_runtime_tag() {
        let m = docker_manifest();
        let js = serde_json::to_string(&m).unwrap();
        assert!(
            js.contains(r#""kind":"docker""#),
            "the runtime kind must be explicit on the wire: {js}"
        );
        let back: Manifest = serde_json::from_str(&js).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn the_url_is_built_from_the_declared_port_and_path() {
        assert_eq!(
            docker_manifest().url(),
            "http://127.0.0.1:8080/v1/chat/completions"
        );
    }

    // A cpuset restricts which cores a process may use but does not change what a runtime believes
    // is available; see `pinned_parallelism`'s doc for why the harness states the core count
    // explicitly rather than trusting each gateway's own env.
    #[test]
    fn every_launched_gateway_is_told_how_many_cores_it_was_pinned_to() {
        // Both launch kinds: a native entrant is pinned with taskset rather than a cpuset and needs
        // telling just as much.
        let mut container = docker_manifest();
        container.launch = Some(LaunchDecl::Docker {
            image: "gw:1".into(),
            env: vec![("GOMAXPROCS".into(), "64".into())],
            args: vec![],
            mounts: vec![],
        });
        let mut native = docker_manifest();
        native.runtime = Runtime::Native {
            proc_match: "gw".into(),
        };
        native.launch = Some(LaunchDecl::Native {
            build: None,
            binary: vec!["gw".into()],
            args: vec![],
            env: vec![],
            env_unset: vec![],
        });
        for (label, m) in [("container", container), ("native", native)] {
            let dir = std::path::Path::new(".");
            let spec = m
                .launch_spec(
                    "0-3",
                    8000,
                    dir,
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                )
                .expect("this manifest declares a launch")
                .expect("and it resolves");
            let env = match &spec.kind {
                crate::launch::LaunchKind::Docker { env, .. } => env,
                crate::launch::LaunchKind::Native { env, .. } => env,
            };
            let got = |k: &str| {
                env.iter()
                    .rev()
                    .find(|(n, _)| n == k)
                    .map(|(_, v)| v.as_str())
            };
            // Read the LAST assignment for each name: the harness appends its values after the
            // gateway's, so a gateway setting its own GOMAXPROCS cannot escape the pinning.
            assert_eq!(got("GOMAXPROCS"), Some("4"), "{label}: GOMAXPROCS");
            assert_eq!(got("UV_THREADPOOL_SIZE"), Some("4"), "{label}: node pool");
            assert_eq!(got("RAYON_NUM_THREADS"), Some("4"), "{label}: rayon");
            assert_eq!(got("OMP_NUM_THREADS"), Some("4"), "{label}: openmp");
        }
    }

    // See `rig_owned_headers_declared`'s doc for the header-collision defect this disclosure exists
    // to name.
    #[test]
    fn a_manifest_restating_a_header_the_rig_owns_is_reported() {
        let m = Manifest {
            headers: vec!["Authorization: Bearer {GW_AUTH}".into()],
            ..docker_manifest()
        };
        let found = m.rig_owned_headers_declared();
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("authorization") && found[0].contains("headers"),
            "the report must name the header and where it was declared: {found:?}"
        );
        // Reported case-insensitively, because HTTP header names are. `found[0]` above is lowercased.

        // Every dialect's, not just the ones this gateway wires up.
        for name in ["x-api-key", "x-goog-api-key", "anthropic-version"] {
            let m = Manifest {
                egress_headers: [("openai".to_string(), vec![format!("{name}: whatever")])]
                    .into_iter()
                    .collect(),
                ..docker_manifest()
            };
            assert_eq!(
                m.rig_owned_headers_declared().len(),
                1,
                "{name} is a header the rig sends itself"
            );
        }

        // A routing header the harness never sends is the normal case and must stay silent.
        let m = Manifest {
            headers: vec!["x-llm-provider: anthropic".into()],
            egress_headers: [(
                "openai".to_string(),
                vec!["x-portkey-custom-host: http://127.0.0.1:{MOCK_PORT}/v1".into()],
            )]
            .into_iter()
            .collect(),
            ..docker_manifest()
        };
        assert!(m.rig_owned_headers_declared().is_empty());
    }
}

#[cfg(test)]
mod real_field_tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Every manifest in the real field, read from the gateways' own directories.
    ///
    /// DISCOVERED, not listed. A single file naming every entrant would be the hand-maintained
    /// roster `lib/gateway_isolation_test.sh` exists to prevent (that lint's scan skips `.json`, so
    /// such a file would slip past it).
    ///
    /// Uses the real field rather than an invented example: a schema that only describes a made-up
    /// case proves nothing about whether it fits the actual entrants.
    fn field() -> BTreeMap<String, Manifest> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../gateways");
        let mut out = BTreeMap::new();
        for entry in std::fs::read_dir(&root)
            .expect("the gateways directory must exist")
            .flatten()
        {
            let def = entry.path().join("definition.json");
            if !def.is_file() {
                continue;
            }
            let m = Manifest::load(&entry.path()).unwrap_or_else(|e| panic!("{e}"));
            out.insert(m.name.clone(), m);
        }
        assert!(!out.is_empty(), "no gateways/*/definition.json found");
        out
    }

    #[test]
    fn every_real_manifest_parses_and_validates() {
        let f = field();
        assert!(
            f.len() >= 13,
            "the whole field should be represented, got {}",
            f.len()
        );
        for (name, m) in &f {
            assert!(
                m.validate().is_ok(),
                "{name} must validate: {:?}",
                m.validate()
            );
            assert_eq!(&m.name, name, "the key and the declared name must agree");
        }
    }

    /// A manifest's `runtime.identity()` is the ONE place its measured identity is spelled; no
    /// second spelling exists to drift out of sync with it.
    #[test]
    fn no_manifest_can_name_two_different_things_to_measure() {
        for (name, m) in &field() {
            let id = m.runtime.identity();
            assert!(
                !id.trim().is_empty(),
                "{name} must declare something measurable"
            );
            // Asserting it twice is the closest a test can get to asserting no second spelling exists.
            assert_eq!(m.runtime.identity(), id);
        }
    }

    /// Every container manifest can produce a real invocation: this walks the real corpus and
    /// builds the actual docker command line for each one.
    #[test]
    fn every_container_manifest_produces_a_launchable_invocation() {
        use std::time::Duration;
        let mut launchable = 0;
        for (name, m) in &field() {
            let spec = m
                .launch_spec(
                    "0-3",
                    8000,
                    std::path::Path::new("/gw"),
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                )
                .unwrap_or_else(|| panic!("{name} must declare how it is launched"))
                .unwrap_or_else(|e| panic!("{name} must produce a launchable spec: {e}"));
            assert!(spec.validate().is_ok(), "{name}: {:?}", spec.validate());

            let inv = crate::launch::build_invocation(&spec);
            // A container is started by the container runtime; a source-built entrant is started
            // pinned, directly. Both must be launchable.
            let expected = if m.runtime.is_docker() {
                "docker"
            } else {
                "taskset"
            };
            assert_eq!(inv.program, expected, "{name}");
            if !m.runtime.is_docker() {
                launchable += 1;
                continue;
            }
            // The container name comes from runtime.identity(), NOT from the launch block, so the
            // thing started, the thing measured and the thing stopped cannot be three containers.
            assert!(
                inv.args
                    .windows(2)
                    .any(|w| w == ["--name".to_string(), m.runtime.identity().to_string()]),
                "{name} must launch under its declared identity: {:?}",
                inv.args
            );
            assert!(
                inv.args
                    .windows(2)
                    .any(|w| w == ["--cpuset-cpus".to_string(), "0-3".to_string()]),
                "{name} must be pinned: {:?}",
                inv.args
            );
            // A mount is resolved against the gateway's own directory: an absolute path in a manifest
            // only works on the machine it was written on.
            for a in &inv.args {
                assert!(
                    !a.contains("$GW_DIR"),
                    "{name} left an unexpanded shell path: {a}"
                );
                assert!(
                    !a.contains('{') || !a.contains('}'),
                    "{name} left an unresolved placeholder: {a}"
                );
            }
            launchable += 1;
        }
        // EVERY discovered entrant, not a literal count. A frozen number here fails the moment the
        // field changes size, which is the same defect `gateways/README.md` records against the old
        // "n/13" footer: the field is DISCOVERED from gateways/*/, so what this asserts is that the
        // walk above reached all of them, not that there are some particular number of them.
        assert_eq!(
            launchable,
            field().len(),
            "every entrant must be launchable, got {launchable} of {}",
            field().len()
        );
    }

    /// PLANO'S TWO BOOT FAILURES, LOCKED SO A BOX IS NEVER SPENT ON THEM AGAIN.
    ///
    /// Both are visible in the files alone (no gateway needs to start), and both used to cost a
    /// launched EC2 box that waited out its readiness attempts and was torn down INCOMPLETE.
    ///
    /// 1. `provider_interface` beside a model prefix Plano already knows: config_generator.py:445
    ///    rejects the pair and the container exits. Its error text ("Please provide provider
    ///    interface as part of model name") reads as the opposite of the rule it enforces.
    /// 2. Listener port 10000: Plano always renders its prompt-gateway listener there, so an LLM
    ///    listener on 10000 double-binds and envoy exits ("'egress_traffic' has duplicate address
    ///    '0.0.0.0:10000'").
    #[test]
    fn planos_config_cannot_regress_to_the_two_forms_it_refuses_to_boot_with() {
        let f = field();
        let Some(m) = f.get("plano") else { return };
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../gateways/plano");

        assert_ne!(
            m.port, 10000,
            "plano's listener port must not be 10000: plano renders its own prompt-gateway listener \
             there, and envoy exits at boot with a duplicate-address error rather than serving"
        );

        for cf in &m.config_files {
            let tmpl = std::fs::read_to_string(dir.join(&cf.template))
                .unwrap_or_else(|e| panic!("plano template {} must be readable: {e}", cf.template));
            for (i, line) in tmpl.lines().enumerate() {
                let code = line.split('#').next().unwrap_or("");
                assert!(
                    !code.contains("provider_interface"),
                    "plano's {} line {} sets provider_interface ({:?}); its model prefixes are \
                     providers plano already knows, and setting both makes config generation exit \
                     and takes the container down with it",
                    cf.template,
                    i + 1,
                    code.trim()
                );
            }
        }
    }

    /// A GATEWAY MAY ONLY CLAIM A CELL WHOSE EGRESS DIALECT IT HAS A MODEL FOR.
    ///
    /// This harness infers upstream dialect from the request PATH, so a gateway that routes without
    /// translating still answers 200 and scores served — e.g. plano's openai>anthropic cell nearly
    /// published a green for a request forwarded in the OpenAI shape.
    ///
    /// Path-inference itself needs a live upstream to check; what CAN be checked cheaply is that a
    /// claimed egress column at least names a model to drive it with.
    #[test]
    fn no_gateway_claims_an_egress_column_it_has_no_model_for() {
        use crate::ingress::Dialect;
        for (name, m) in &field() {
            // A manifest naming no models leaves selection to the harness default; this asserts the
            // mixed case, where SOME columns are named and a claimed one isn't.
            if m.egress_models.is_empty() {
                continue;
            }
            for row in &m.matrix {
                for (col, ch) in row.chars().enumerate() {
                    if ch != '1' {
                        continue;
                    }
                    let egress = Dialect::ALL[col].as_str();
                    assert!(
                        m.egress_models.contains_key(egress),
                        "{name} claims an egress column it cannot drive: the matrix asserts a \
                         {egress} upstream cell, but egress_models names no {egress} model, so the \
                         cell would be measured with whatever model the default picks"
                    );
                }
            }
        }
    }

    /// The Go runtime's thread count is set from the size of the pinned core range, not the host's —
    /// the core split IS the comparability basis of every number on the board.
    #[test]
    fn the_core_count_placeholder_resolves_to_the_pinned_range_not_the_host() {
        use std::time::Duration;
        let f = field();
        let with_ncore: Vec<_> = f
            .iter()
            .filter(|(_, m)| {
                m.launch.as_ref().is_some_and(|l| match l {
                    LaunchDecl::Docker { env, .. } | LaunchDecl::Native { env, .. } => {
                        env.iter().any(|(_, v)| v.contains("{NCORE}"))
                    }
                })
            })
            .collect();
        assert!(
            !with_ncore.is_empty(),
            "some entrants set their thread count from the core pin"
        );

        for (name, m) in with_ncore {
            for (cores, expected) in [("0-3", "4"), ("4-9", "6"), ("2", "1")] {
                let spec = m
                    .launch_spec(
                        cores,
                        8000,
                        std::path::Path::new("/gw"),
                        Duration::from_secs(1),
                        Duration::from_secs(1),
                    )
                    .and_then(Result::ok)
                    .unwrap_or_else(|| panic!("{name} must build a spec"));
                let crate::launch::LaunchKind::Docker { env, .. } = &spec.kind else {
                    panic!("{name} is a container entrant")
                };
                let v = env
                    .iter()
                    .find(|(k, _)| k == "GOMAXPROCS")
                    .map(|(_, v)| v.as_str());
                assert_eq!(
                    v,
                    Some(expected),
                    "{name} on cores {cores} must run {expected} threads"
                );
            }
        }
    }

    /// THE THIRTY-SIX CELL DEFECT, as a test — see `LaunchDecl::Native::env_unset`'s doc for the
    /// underlying mechanism (a config loader rejecting unknown env vars, killing config load
    /// silently since the launch still reports success).
    #[test]
    fn a_native_entrant_can_require_that_a_variable_is_absent_not_merely_unset_by_us() {
        use std::time::Duration;
        let f = field();
        let scrubbing: Vec<_> = f
            .iter()
            .filter(|(_, m)| {
                m.launch.as_ref().is_some_and(
                    |l| matches!(l, LaunchDecl::Native { env_unset, .. } if !env_unset.is_empty()),
                )
            })
            .collect();
        assert!(
            !scrubbing.is_empty(),
            "at least one entrant must scrub the environment it is launched with; that requirement is why this field exists"
        );

        for (name, m) in scrubbing {
            let spec = m
                .launch_spec(
                    "0-3",
                    8000,
                    std::path::Path::new("/gw"),
                    Duration::from_secs(1),
                    Duration::from_secs(1),
                )
                .and_then(Result::ok)
                .unwrap_or_else(|| panic!("{name} must build a spec"));
            let inv = crate::launch::build_invocation(&spec);
            assert!(
                !inv.env_unset.is_empty(),
                "{name} declares variables that must not reach it, and the invocation must carry that removal: {inv:?}"
            );
            // An empty value is still a present variable, and a loader rejecting unknown KEYS
            // doesn't care what the value is.
            for removed in &inv.env_unset {
                assert!(
                    !inv.env.iter().any(|(k, _)| k == removed),
                    "{name} both sets and removes {removed}; setting it to anything still leaves it present"
                );
            }
        }
    }

    /// A doubled brace is a literal one, BOTH ways round: handling only the opening half breaks a
    /// YAML `error_map: {}`, rendering it as `{}}`.
    /// A VALIDATOR THAT ONLY EVER SAYS OK HAS NOT BEEN SHOWN TO WORK: the real entrants passing
    /// proves nothing on its own, so this test injects the mistakes each check exists to catch.
    #[test]
    fn every_setup_mistake_the_validator_exists_for_is_actually_reported() {
        let dir = std::env::temp_dir().join(format!("otb-validate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        std::fs::write(dir.join("typo.tmpl"), "port: {NOT_A_REAL_THING}\n")
            .expect("write template");

        let mut m = field()
            .values()
            .find(|m| m.runtime.is_docker())
            .cloned()
            .expect("an entrant");
        m.config_files = vec![
            ConfigFile {
                template: "missing.tmpl".into(),
                output: "a.yaml".into(),
            },
            ConfigFile {
                template: "typo.tmpl".into(),
                output: "b.yaml".into(),
            },
        ];
        m.launch = Some(LaunchDecl::Docker {
            image: "x:1".into(),
            env: vec![],
            args: vec![],
            mounts: vec![MountDecl {
                host_path: "never-rendered.yaml".into(),
                container_path: "/c.yaml".into(),
                read_only: true,
            }],
        });
        m.egress_headers = [("notadialect".to_string(), vec!["x-a: b".to_string()])]
            .into_iter()
            .collect();

        let problems = m.problems(&dir);
        let joined = problems.join("\n");

        assert!(
            joined.contains("missing.tmpl"),
            "a declared template that is absent must be reported: {joined}"
        );
        assert!(
            joined.contains("NOT_A_REAL_THING"),
            "a placeholder the harness cannot supply must be reported: {joined}"
        );
        assert!(
            joined.contains("never-rendered.yaml"),
            "a mount nothing renders must be reported - this is the one that costs the most to diagnose, because the container just exits: {joined}"
        );
        assert!(
            joined.contains("notadialect"),
            "a header keyed by a dialect we do not speak must be reported: {joined}"
        );

        // The message has to tell a maintainer what to DO, not just that something is wrong.
        assert!(
            joined.contains("MOCK_PORT"),
            "the placeholder error must list what IS available: {joined}"
        );

        // A correct gateway stays quiet, or the validator is noise and gets ignored.
        for (name, real) in &field() {
            let gw_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../gateways")
                .join(name);
            assert!(
                real.problems(&gw_dir).is_empty(),
                "{name} is a real entrant and must validate clean: {:?}",
                real.problems(&gw_dir)
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_literal_brace_survives_the_render_intact() {
        let m = field().values().next().cloned().expect("an entrant");
        let dir = std::path::Path::new("/gw");
        let go = |t: &str| m.substitute(t, "0-3", 8000, dir).expect("renders");

        assert_eq!(go("error_map: {{}}"), "error_map: {}");
        assert_eq!(go("{{\"a\": 1}}"), "{\"a\": 1}");
        // A placeholder still resolves when it sits beside literal braces.
        assert_eq!(go("{{port: {MOCK_PORT}}}"), "{port: 8000}");
        // A lone closing brace is content; nothing opened it.
        assert_eq!(go("a} b"), "a} b");
    }

    #[test]
    fn a_launch_referring_to_something_the_harness_does_not_supply_is_refused() {
        use std::time::Duration;
        // Built from the real corpus rather than a fixture, which could drift from the real shape.
        let mut m = field()
            .values()
            .find(|m| m.runtime.is_docker())
            .cloned()
            .expect("a container entrant");
        m.launch = Some(LaunchDecl::Docker {
            image: "gw:1".into(),
            env: vec![("X".into(), "{NOT_A_THING}".into())],
            args: vec![],
            mounts: vec![],
        });
        let err = m
            .launch_spec(
                "0-3",
                8000,
                std::path::Path::new("/gw"),
                Duration::from_secs(1),
                Duration::from_secs(1),
            )
            .and_then(Result::err);
        assert!(
            matches!(err, Some(ManifestError::UnknownPlaceholder { ref name, .. }) if name == "NOT_A_THING"),
            "an unknown placeholder must be refused, not passed through as a literal: {err:?}"
        );
    }

    /// EVERY DECLARED CONFIG TEMPLATE ACTUALLY RENDERS. A template referring to something the
    /// harness does not supply produces a gateway that starts and immediately dies — this fails
    /// here, at the manifest, rather than there.
    #[test]
    fn every_declared_config_template_renders_with_nothing_left_unresolved() {
        let mut rendered = 0;
        for (name, m) in &field() {
            if m.config_files.is_empty() {
                continue;
            }
            let gw_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../gateways")
                .join(name);
            let out =
                std::env::temp_dir().join(format!("otb-render-{}-{}", name, std::process::id()));
            std::fs::create_dir_all(&out).expect("scratch dir");

            for file in &m.config_files {
                let template_path = gw_dir.join(&file.template);
                let raw = std::fs::read_to_string(&template_path).unwrap_or_else(|e| {
                    panic!(
                        "{name} declares {} which cannot be read: {e}",
                        file.template
                    )
                });
                let body = m
                    .substitute(&raw, "0-3", 8000, &gw_dir)
                    .unwrap_or_else(|e| panic!("{name} template {}: {e}", file.template));

                // Deliberately NOT scanned for leftover braces: a rendered literal `{{...}}` comes
                // out as `{...}`, indistinguishable from an unresolved placeholder by inspection.
                // The guarantee lives in `substitute`, which refuses an unknown name outright.
                assert!(
                    !body.trim().is_empty(),
                    "{name} rendered {} to nothing",
                    file.output
                );
                rendered += 1;
            }
            let _ = std::fs::remove_dir_all(&out);
        }
        assert!(
            rendered >= 13,
            "every declared template must render, got {rendered}"
        );
    }

    #[test]
    fn both_runtime_kinds_are_present_in_the_real_field() {
        let f = field();
        assert!(
            f.values().any(|m| m.runtime.is_docker()),
            "some entrants run in containers"
        );
        assert!(
            f.values().any(|m| !m.runtime.is_docker()),
            "some entrants run natively from source"
        );
    }

    #[test]
    fn every_manifest_declares_a_reachable_url() {
        for (name, m) in &field() {
            let u = m.url();
            assert!(
                u.starts_with("http://127.0.0.1:"),
                "{name} must be driven on loopback, got {u}"
            );
            assert!(u.contains(&m.port.to_string()));
        }
    }
}

#[cfg(test)]
mod unresolvable_header_tests {
    use super::*;

    // A HEADER THAT CANNOT RESOLVE MUST NOT SILENTLY BECOME NO HEADERS.
    //
    // `validate()` rejects a shell-style `$` but never exercises `{...}` substitution, so a header
    // like `Authorization: Bearer {NOT_A_PLACEHOLDER}` passes validate and only fails later inside
    // `headers_for`. That failure used to be swallowed by `.unwrap_or_default()` in
    // `suite::run_suite_with`, measuring the run with an EMPTY header set and publishing
    // `served: false` — blaming the gateway for the harness dropping its own credentials.
    //
    // This pins both halves: validate() does NOT catch it, and headers_for DOES return Err.
    #[test]
    fn an_unknown_placeholder_in_a_header_escapes_validate_and_fails_resolution() {
        let m = Manifest {
            headers: vec!["Authorization: Bearer {NOT_A_REAL_PLACEHOLDER}".to_string()],
            ..test_fixture()
        };
        assert!(
            m.validate().is_ok(),
            "validate() only rejects shell-style `$`; if it ever learns to resolve braces, the \
             refusal in run_suite_with becomes redundant and this test should say so"
        );
        let resolved = m.headers_for("", "0-3", 9000, std::path::Path::new("/tmp"));
        assert!(
            resolved.is_err(),
            "an unknown placeholder must fail resolution - otherwise the run proceeds with a header \
             whose value still contains literal braces"
        );
    }

    // Must still resolve, or the refusal above would be indistinguishable from a system that never
    // works at all.
    #[test]
    fn a_manifest_with_no_placeholders_resolves_its_headers() {
        let m = Manifest {
            headers: vec!["X-Fixed: value".to_string()],
            ..test_fixture()
        };
        let got = m
            .headers_for("", "0-3", 9000, std::path::Path::new("/tmp"))
            .expect("a header with no placeholder resolves");
        assert!(
            got.iter().any(|(k, v)| k == "X-Fixed" && v == "value"),
            "the resolved set must contain the declared header, got {got:?}"
        );
    }
}

#[cfg(test)]
mod unknown_field_tests {
    use super::*;

    /* A MANIFEST KEY NOBODY READS MUST FAIL AT LOAD. Without deny_unknown_fields, `runtime.image`
    (`Runtime::Docker` has no such field; the image lives on `launch`) would parse cleanly and
    change nothing — the cost of the silent version is a wasted run; the loud version costs a parse
    error naming the field. */
    #[test]
    fn an_image_written_where_the_schema_has_none_is_refused() {
        let json = r#"{
            "name": "x", "display": "X", "lang": "Rust", "class": "Gateway",
            "port": 8000, "model": "m", "auth": "t",
            "runtime": { "kind": "docker", "container": "c", "image": "getbusbar/busbar:9.9.9" },
            "egress": ["openai"]
        }"#;
        let err = serde_json::from_str::<Manifest>(json)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("image"),
            "the error must name the field that was ignored, got: {err}"
        );
    }

    /// And a stray top-level key is refused for the same reason - a typo'd or stale field is a
    /// defect whichever level it sits at.
    #[test]
    fn a_stray_top_level_key_is_refused() {
        let json = r#"{
            "name": "x", "display": "X", "lang": "Rust", "class": "Gateway",
            "port": 8000, "model": "m", "auth": "t",
            "runtime": { "kind": "docker", "container": "c" },
            "egress": ["openai"],
            "verison": "1.5.0"
        }"#;
        let err = serde_json::from_str::<Manifest>(json)
            .unwrap_err()
            .to_string();
        assert!(err.contains("verison"), "got: {err}");
    }

    /// The real manifests must still parse - a guard that rejects the field is worthless if it also
    /// rejects the board.
    #[test]
    fn every_shipped_manifest_still_parses_under_the_stricter_schema() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../gateways");
        let mut n = 0;
        for e in std::fs::read_dir(&dir).expect("gateways/") {
            let p = e.expect("entry").path().join("definition.json");
            if !p.exists() {
                continue;
            }
            let text = std::fs::read_to_string(&p).expect("read");
            serde_json::from_str::<Manifest>(&text)
                .unwrap_or_else(|err| panic!("{} no longer parses: {err}", p.display()));
            n += 1;
        }
        assert!(n >= 14, "expected the whole field, saw {n}");
    }
}
