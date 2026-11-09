// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// A gateway manifest, as DATA.
//
// Identity is declared ONCE, not spelled out separately per reader: a manifest that named its
// container or process pattern once per hook (once for RSS, once for HWM, once for stop) could drift
// so RSS reads one process while HWM reads that process and every descendant, publishing two
// different populations side by side for the same gateway - a gateway that forks workers would have
// its peak inflated relative to its idle by whatever its children weighed. Every reader deriving
// from one declaration removes that class of bug rather than merely guarding against it.

use serde::{Deserialize, Serialize};

/// How the gateway runs, and therefore how its process tree is found. This is the single declaration
/// every memory reader and the stop path derive from.
/* AN UNKNOWN KEY IS A SILENT NO-OP, AND THAT COSTS A RUN.
`Runtime::Docker` has no `image` field - the image lives on `launch` - but serde ignored unknown
keys, so a `runtime.image` written into a manifest parsed cleanly, was read by nothing, and
changed nothing. On 2026-08-02 that produced a box running getbusbar/busbar:1.4.1 for 25 minutes
under the gateway key `busbar-150`, which would have published 1.4.1's numbers in a row labelled
1.5.0 and shown two columns of the same binary as if they were a version comparison. Nothing in
the harness objected, because from serde's point of view nothing was wrong.

A manifest field nobody reads is either a typo or a stale key, and both should fail at load where
the fix is cheap - not at publish, where the evidence is a box that no longer exists. */
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Runtime {
    /// A container. The root pid comes from the container runtime, and the tree walk starts there.
    Docker {
        /// The name as the manifest declares it, stable across runs.
        container: String,
        /// THE RUN THAT OWNS THIS CONTAINER, appended to the declared name to form the real one.
        ///
        /// A container's `--name` used to be the declared name alone, which is the same string on
        /// every run of that gateway on that box. Two overlapping runs then name ONE container: the
        /// second run's `docker run` collides, and its retry loop's `stop()` is `docker rm -f` on
        /// that shared name, which deletes the FIRST run's container in the middle of its
        /// measurement. Scoping the name means a run can only ever remove its own.
        ///
        /// Not in any manifest and never serialized back into one: it is assigned per invocation
        /// (`Runtime::scoped_to_run`), the same way run-on-ec2.sh tags the boxes it launched with
        /// `run=$RUN_ID` so its teardown filters to its own and leaves a peer run's alone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_scope: Option<String>,
    },
    /// A process started directly on the box, located by a match against its command line.
    Native { proc_match: String },
}

impl Runtime {
    /// The one identity string, whatever the kind. Readers take this rather than being handed a name
    /// per call site, which is what makes a mismatch between them unrepresentable. For a container
    /// this is the RUN-SCOPED name: the name that was created, that gets measured, and that gets
    /// removed, all three from here.
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

    /// The identity as the MANIFEST spells it, without a run scope. For validation messages and for
    /// the `otb.gateway` label, where the stable name is the useful one; never for naming, finding or
    /// removing a container, which must all go through `identity`.
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
    /// A NATIVE identity is returned UNCHANGED, and that is not an oversight: `proc_match` matches a
    /// command line the gateway itself produces, and no run id appears in it. The isolation a native
    /// gateway needs comes from the port it binds, which two overlapping runs cannot share anyway.
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

/// A run id reduced to what a container name accepts (`[a-zA-Z0-9_.-]`), because the scope is
/// concatenated into `--name` and a rejected name is a launch that never happens. `None` for a scope
/// that survives as nothing, so an unusable id leaves the name unscoped rather than trailing a bare
/// separator.
fn sanitize_run_scope(run_id: &str) -> Option<String> {
    let cleaned: String = run_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
        .collect();
    let cleaned = cleaned.trim_matches(['.', '-', '_']).to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Command-line fragments too generic to name one process. Matching is a SUBSTRING against every
/// full command line on the box (`supervise::select_matches`), so a `proc_match` like `sh` or `node`
/// selects a crowd, and whichever member of that crowd the stop path signals or the memory reader
/// sums is a coin toss the artifact reports as the gateway's.
const GENERIC_PROC_MATCHES: [&str; 14] = [
    "sh", "bash", "python", "python3", "node", "java", "docker", "server", "proxy", "gateway",
    "main", "app", "run", "start",
];

/// The shortest a `proc_match` may be. Four characters of a command line is not an identity; every
/// entrant in the field declares a path or a full binary name, which is what this asks for.
const MIN_PROC_MATCH_LEN: usize = 8;

/// Why a declared `proc_match` cannot be trusted to name exactly one process, or `None` if it can.
///
/// Checked at manifest level rather than only at match time because a generic pattern's damage is
/// silent: the wrong process is stopped, or a bystander's memory is published as the gateway's, and
/// both read as a plausible number rather than as an error.
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
    // The engine's own binary. A pattern that appears in the harness's argv makes the harness a
    // candidate for its own stop signal, and makes `is_alive` read the engine's command line as
    // proof the gateway is still up.
    if m.contains("otb") {
        return Some(format!(
            "proc_match {m:?} contains the engine's own binary name, so it can match the harness's command line rather than the gateway's"
        ));
    }
    None
}

/// Why a config setting exists. The board's fairness rule is that every gateway config is the bare
/// minimum required to run, so each setting must name which necessity it satisfies. As shell this
/// was a free-text block a lint grepped; as an enum the build cannot express a setting with no
/// reason, and "we turned a feature on" has no variant to hide in.
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
// deny_unknown_fields: same rule as `Manifest`/`Runtime` - a key nobody reads must fail at load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigSetting {
    pub key: String,
    pub reason: ConfigReason,
    /// Free text for a human, never load bearing.
    #[serde(default)]
    pub note: String,
}

// Same rule as `Runtime` above, for the same reason: a key the schema does not define is a defect,
// and one that parses is a defect nobody sees.
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
    /// The grid's two axes are driven by two different things. Ingress is the WIRE SHAPE, and the
    /// engine owns that entirely: the path and the body come from `Dialect`. Egress is the UPSTREAM,
    /// and most gateways choose it from the model name in the request, so a cell that does not vary
    /// the model does not vary the egress at all - it sends a byte-identical request to the diagonal
    /// and gets the same upstream, while the artifact labels it a translation. Six columns collapse
    /// into one measurement published six times.
    ///
    /// The engine always asks for the egress's model (`model_for`); this map is only how each
    /// gateway spells it, because the spelling is not ours to choose. Some gateways route on a name
    /// we configure and can be given any label; others infer the provider FROM the name by their own
    /// convention, so the value has to be that convention's spelling or the gateway routes somewhere
    /// else. Absent for an egress means `model` - correct for a gateway with one upstream, and for
    /// the openai column whose canonical name usually IS `model`.
    #[serde(default)]
    pub egress_models: std::collections::BTreeMap<String, String>,
    pub auth: String,
    #[serde(default)]
    pub headers: Vec<String>,
    pub runtime: Runtime,
    /// Egress dialects the manifest configures. This only says which upstreams are wired;
    /// `matrix` declares support and `untestable` identifies rig limitations.
    #[serde(default)]
    pub egress: Vec<String>,
    /// THE DECLARED 6x6 CAPABILITY GRID: which (ingress, egress) pairings this gateway is expected
    /// to serve at all, researched against its own source/docs rather than guessed. Six rows
    /// (ingress) of six characters (egress), axis order `["openai", "openai-responses",
    /// "anthropic", "gemini", "cohere", "bedrock"]` both ways - the same order `Dialect::all()`
    /// walks. `'1'` = declared capable, `'0'` = declared not. Empty (the default) means undeclared:
    /// every cell is probed with no cell skipped on this basis.
    ///
    /// A `'0'` cell is NEVER PROBED AT ALL, published `not_configurable` directly. Probing it anyway
    /// and trusting the returned status would be the exact defect this field exists to prevent: a
    /// gateway's own global auth gate or rate limiter can answer a request for a pairing it never
    /// claims to support with a real HTTP status (401, 429, ...) that has nothing to do with that
    /// specific pairing, because it fires before routing ever gets a chance to say "no such route".
    /// Grading that status as a probed failure would publish the gateway's own front-door behaviour
    /// as a genuine capability defect on a translation it never offered. A `'1'` cell, or any cell
    /// when this field is empty, is probed. A `'1'` cell's rejection is `Failed` even for 404/501;
    /// those statuses cannot erase researched support. Undeclared cells retain the legacy status
    /// classification (see `probe::persistent_transient_verdict`).
    #[serde(default)]
    pub matrix: Vec<String>,
    /// The cited, source-referenced reasoning behind `matrix`'s `'0'` cells, shown to a reader
    /// instead of a bare grey square.
    #[serde(default)]
    pub matrix_note: String,
    /// Cells the RIG cannot pose at all, distinct from a declared incapability: the gateway serves
    /// this pairing in production, but the harness's own mock cannot stand in for the real upstream
    /// (e.g. a channel that signs requests straight to a fixed real hostname with no override).
    /// `"<ingress>/<egress>"` per entry. Also never probed; published `untestable`, not graded either
    /// way.
    #[serde(default)]
    pub untestable: Vec<String>,
    #[serde(default)]
    pub untestable_note: String,
    /// A PER-CELL INGRESS PATH, for the cells that have one. Keyed `"<ingress>>egress"`.
    ///
    /// Some gateways expose a route that skips translation when the client's dialect already matches
    /// the upstream's, and route it differently otherwise. Where that is what an operator would
    /// actually call, measuring the other route measures work the gateway would not have done.
    ///
    /// Absent for a cell means the gateway's declared `path`, which means the dialect's standard
    /// path. Twelve of the thirteen entrants set none of these.
    ///
    /// Whatever is used is RECORDED in the cell it produced, because a number taken on a
    /// provider-pinned route and a number taken on the unified route are different measurements and
    /// the board must not present them as the same one.
    #[serde(default)]
    pub cell_paths: std::collections::BTreeMap<String, String>,
    /// COMMANDS RUN AFTER THE GATEWAY IS UP, one per line, from a file named `commands`.
    ///
    /// Discovered by filename like `env` and `headers.json`, never declared, so every gateway is
    /// described the same way and a reader can see the whole deployment by listing the directory.
    ///
    /// This exists for one shape of gateway: the kind with no config file at all, which stores its
    /// configuration in a database and is configured through its own admin API after it boots. There
    /// is no file to drop for those, so the only honest way to deploy one the way its operators do
    /// is to make the same calls its documentation tells you to make. Almost every gateway has no
    /// `commands` file and needs none.
    ///
    /// Lines are run in order, each through a shell, after the gateway answers as ready and before
    /// anything is measured. A line that fails fails the run: a gateway configured halfway is worse
    /// than one that never started, because it will answer probes and publish numbers for an
    /// upstream that was never wired up.
    #[serde(default, skip)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub config: Vec<ConfigSetting>,
    /// Headers that select the EGRESS, keyed by dialect.
    ///
    /// Some gateways route by config; others route only by a request header, and for those the
    /// header IS the egress column. Without a per-column home every column would be driven
    /// identically, every one would answer 200, and the board would publish four to six identical
    /// columns as though they were different upstream dialects - a wrong number rather than a
    /// missing one, and the kind that looks entirely plausible.
    ///
    /// Values admit the same closed set as everything else, because one gateway's routing header
    /// carries the rig-assigned mock port.
    #[serde(default)]
    pub egress_headers: std::collections::BTreeMap<String, Vec<String>>,
    /// Values this gateway's own templates refer to, beyond the closed set the harness supplies.
    ///
    /// A manifest declares its model name, its upstream URL, its bedrock path ONCE here, and every
    /// template that needs it refers to it by name. That is the point: the shell had these as
    /// variables read by several places each, and one of them - a model spelled in a route URI and
    /// again in a probe path - is documented as having cost thirty-six cells when the two drifted.
    ///
    /// Values may themselves refer to the closed set, so `"url": "http://127.0.0.1:{MOCK_PORT}"`
    /// resolves at render time rather than being frozen at whatever port a previous run used.
    #[serde(default)]
    pub constants: std::collections::BTreeMap<String, String>,
    /// Config files the harness renders and the gateway reads.
    ///
    /// A TEMPLATE FILE in the gateway's own directory, not a string in this manifest and not Rust.
    /// The gateway-isolation lint (`lib/gateway_isolation_test.sh`, since deleted in 1127cf1) exempted
    /// files under `gateways/<name>/` from Rule 1 but scanned `.rs` everywhere, so a Rust function rendering a config that must contain the gateway's own
    /// name as a top-level key - which at least one does - could not exist. As a file beside the
    /// gateway it is legal, readable, and diffable against what the gateway actually booted with.
    #[serde(default)]
    pub config_files: Vec<ConfigFile>,
    /// How to START this gateway. `None` for a manifest that only describes a gateway someone else
    /// is running - which is every manifest today, because nothing in the tree could launch one.
    #[serde(default)]
    pub launch: Option<LaunchDecl>,
}

/// One config file: a template beside the gateway, and where the rendered result goes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigFile {
    /// Template path, relative to the gateway's own directory.
    pub template: String,
    /// Rendered output, relative to the same directory. This is what a mount points at.
    pub output: String,
}

/// A file the gateway reads, rendered by the harness and handed to it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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

/// Whether a path is a file this box can execute.
///
/// Used to pick among declared binary candidates. On a non-unix host the mode bits are unavailable,
/// so existence is the best available answer; the field runs on Linux and the distinction only
/// matters there.
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

/// How many cores a cpuset string covers, e.g. "0-3" is four and "0-3,8-11" is eight.
///
/// The value the Go runtime and the other pools are pinned to (`pinned_parallelism`, `{NCORE}`).
/// It reads the cpuset with the SAME grammar the utilisation reading does (`procsample::parse_cores`:
/// a comma list of cores and ranges, as taskset and `--cpuset-cpus` accept), so the parallelism the
/// harness states can never disagree with the cores it pinned. Only a plain "lo-hi" range used to be
/// understood, and a list was silently one core. Nothing parseable is one, because claiming more
/// parallelism than the pin allows is the direction that corrupts a measurement.
fn core_count(cores: &str) -> u32 {
    u32::try_from(crate::procsample::parse_cores(cores).len())
        .unwrap_or(u32::MAX)
        .max(1)
}

/// Everything needed to start one gateway, as DATA.
///
/// deny_unknown_fields for the reason `Runtime` carries it: `launch` is where image, env, args and
/// mounts live, so a typo'd or misplaced key here is exactly the silent no-op that once measured the
/// wrong binary under another version's label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
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
    /// Built from source and run directly on the box. Three entrants.
    Native {
        /// A script in the gateway's own directory that produces the binary, run once before the
        /// first launch and never between egress columns. It installs a toolchain, so it must not be
        /// running during the measurement window: the shell ordered the build before the memory
        /// baseline for exactly that reason.
        #[serde(default)]
        build: Option<String>,
        /// Candidate paths to the built binary, first one that exists and is executable wins.
        ///
        /// A LIST because one entrant's crate does not emit a stable output name and the shell had to
        /// `find` across three of them. Declaring the candidates keeps that as data a reader can see,
        /// rather than a search whose result nothing records.
        binary: Vec<String>,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: Vec<(String, String)>,
        /// Names REMOVED from the child environment before it starts.
        ///
        /// NOT hygiene. One entrant's config loader claims every variable sharing its prefix and
        /// feeds it to a deny-unknown-fields deserializer, so the harness's own documented override
        /// variables kill config load before the port binds. The binary is backgrounded, so the
        /// launch still returns success and the only symptom is "port not listening" on every
        /// attempt of every column.
        ///
        /// `std::process::Command` inherits the parent environment, and an env block can only ADD, so
        /// without this the class is unrepresentable.
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
/// Removal is not a convenience. One entrant's config loader claims every variable sharing its
/// prefix and feeds it to a deny-unknown-fields deserializer, so the harness's own override
/// variables kill config load before the port binds - and the process is backgrounded, so the launch
/// reports success and the only symptom is a port that never listens. An env block that can only ADD
/// cannot express it.
///
/// Deliberately parsed, never executed: nothing in the measurement path runs untrusted config.
///
/// A LINE THAT IS NEITHER IS REFUSED, naming it. It used to be skipped, and a shell-style line was
/// taken literally (`export FOO=bar` set a variable named "export FOO"): either way the gateway ran
/// without a variable its env file declares, which surfaces as a gateway that boots wrong rather
/// than as a setup error. Values are taken verbatim (quotes included), as `docker --env-file` does.
fn parse_env(raw: &str) -> Result<EnvSidecar, String> {
    let mut set = Vec::new();
    let mut unset = Vec::new();
    for (i, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let name_ok = |n: &str| !n.is_empty() && !n.contains(char::is_whitespace);
        if let Some(name) = line.strip_prefix('-') {
            let name = name.trim();
            if !name_ok(name) {
                return Err(format!(
                    "line {}: {line:?} does not name one variable to remove",
                    i + 1
                ));
            }
            unset.push(name.to_string());
        } else if let Some((k, v)) = line.split_once('=').filter(|(k, _)| name_ok(k.trim())) {
            set.push((k.trim().to_string(), v.trim().to_string()));
        } else {
            return Err(format!(
                "line {}: {line:?} is neither NAME=value nor -NAME, so it would set nothing",
                i + 1
            ));
        }
    }
    Ok((set, unset))
}

/// A parsed env sidecar: the variables it sets, and the names it removes.
type EnvSidecar = (Vec<(String, String)>, Vec<String>);

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
    /// A field still carrying a shell variable that was never expanded. These manifests were
    /// EXTRACTED from shell, where one gateway writes `GW_MODEL="$SOME_MODEL"` - one indirection away
    /// from the literal it resolves to. Extract the wrong side of that and the field holds the
    /// reference instead of a model name. Non-empty, so every existing check passes it.
    UnexpandedVariable {
        field: &'static str,
        raw: String,
    },
    /// A config setting with no stated necessity cannot be lint-checked, so it cannot ship.
    ConfigWithoutReason(String),
    /// A launch declaration refers to something the harness does not supply. Loud rather than passed
    /// through: a `{TYPO}` reaching a container as a literal is a misconfiguration that boots and
    /// measures fine, and publishes a number taken under the wrong settings.
    UnknownPlaceholder {
        name: String,
        raw: String,
    },
    /// A declared constant refers to itself, directly or through a ring of others.
    ConstantCycle {
        name: String,
    },
    /// A native `proc_match` too generic to name one process. Refused at load rather than at match
    /// time: by then the wrong process has already been signalled or measured, and the result looks
    /// like a number rather than like a fault.
    IndistinctProcMatch(String),
    /// A declared `matrix` that is not the 6x6 grid of '0'/'1' it documents. Any other byte used to
    /// read as a declared '0', and a '0' cell is never probed and publishes `not_configurable`, so a
    /// typo silently erased a cell the gateway may well serve.
    BadMatrix(String),
    /// A header line with no ':' separating name from value. It could only be dropped, and a gateway
    /// measured without a header its own manifest declared publishes as one that serves nothing.
    HeaderWithoutColon(String),
}

/// How deep a constant may refer to other constants before it is treated as a cycle. One real chain
/// exists (a path built from a model name); anything much deeper is a mistake, not a design.
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
            ManifestError::BadMatrix(why) => write!(f, "matrix {why}"),
            ManifestError::HeaderWithoutColon(line) => write!(
                f,
                "header {line:?} has no colon, so it cannot be sent - write it as it appears on the \
                 wire, \"Name: value\""
            ),
            ManifestError::ConstantCycle { name } => {
                write!(f, "constant {name:?} refers to itself, directly or through a ring of others")
            }
            // Deliberately does NOT say where: this same error is raised for a launch declaration,
            // a config template and a header value, and the caller knows which of those it was
            // reading. Naming the wrong one is worse than naming none.
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
/// The harness decides how many cores a gateway gets. It follows that the harness, not the gateway,
/// must tell the gateway's runtime how many it got, and must tell every gateway the same way.
///
/// Pinning alone is not enough, and the gap is silent. A cpuset restricts which cores a process may
/// run on; it does not change what a runtime THINKS is available. Some runtimes ask the kernel for
/// their affinity and get the right answer; others read the machine's online CPU count and size
/// their thread pools for a sixteen core box while confined to four. That gateway then runs with
/// four times the threads it should have, contends with itself, and publishes a number that
/// describes a configuration no operator would deploy.
///
/// Set centrally rather than left to each gateway's own env block: a per-gateway setting is easy to
/// drop silently, since a missing one still lets the gateway boot with nothing to signal the mistake.
///
/// The list is deliberately runtime-standard names only. A gateway whose runtime has its own knob
/// declares it in its own env with `{NCORE}`, because naming one entrant's variable in shared code
/// would be per-gateway logic in the one place that must not have any.
fn pinned_parallelism(ncore: u32) -> Vec<(String, String)> {
    let n = ncore.to_string();
    [
        // Go: honours affinity already, set anyway so the value is explicit and identical everywhere.
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

/// Why a declared `matrix` is not the documented grid, or `None` if it is (or is undeclared).
///
/// Empty means undeclared. Otherwise it must be one row per `Dialect::ALL` ingress, each exactly one
/// '0'/'1' per egress: `matrix_declared_capable` reads every other byte as a declared '0', and a
/// short row as undeclared, so a malformed grid would silently skip or probe the wrong cells.
fn matrix_shape_problem(matrix: &[String]) -> Option<String> {
    let n = crate::ingress::Dialect::ALL.len();
    if matrix.is_empty() {
        return None;
    }
    if matrix.len() != n {
        return Some(format!(
            "has {} rows; it must have {n} (one per ingress dialect)",
            matrix.len()
        ));
    }
    for (i, row) in matrix.iter().enumerate() {
        if row.len() != n || !row.bytes().all(|b| b == b'0' || b == b'1') {
            return Some(format!(
                "row {i} is {row:?}; every row must be exactly {n} characters of '0' or '1'"
            ));
        }
    }
    None
}

/// This cell's declared capability, from a manifest's `matrix` (six rows of six `'0'`/`'1'` chars,
/// axis order `Dialect::ALL` both ways). `None` means undeclared (probe normally - backward
/// compatible with a manifest that sets no `matrix` at all); `Some(bool)` is the declared answer,
/// and a `Some(false)` cell must never be probed - see `Manifest::matrix`'s own doc for why.
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

        // An extraction artefact, not a typo, and the reason it needs its own check: a field holding
        // an unexpanded reference is non-empty, so every check above passes it, and it survives all
        // the way to the wire. A model name that is really a shell reference is sent as the request body's model,
        // the gateway rejects it against the model its own route declares, and `probe.rs` classifies
        // any status from a healthy rig as `NotConfigured` - "the gateway answered, deterministically,
        // that this pairing does not light up". The board then publishes OUR extraction bug as that
        // gateway's own capability denial. No legitimate value of any of these fields contains `$`.
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
        // The same wire, the same rationale: per-egress headers (headers.json) and per-cell paths
        // reach the gateway exactly as `headers` do.
        for (h, field) in self
            .headers
            .iter()
            .map(|h| (h, "headers"))
            .chain(
                self.egress_headers
                    .values()
                    .flatten()
                    .map(|h| (h, "egress_headers")),
            )
            .chain(self.cell_paths.values().map(|p| (p, "cell_paths")))
        {
            if h.contains('$') {
                return Err(ManifestError::UnexpandedVariable {
                    field,
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
        // A NATIVE IDENTITY MUST BE DISTINCTIVE, checked here rather than trusted at match time. The
        // stop path signals, and the memory reader sums, whatever command lines contain this string;
        // a generic one selects a bystander and publishes its memory - or kills it - under this
        // gateway's name.
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
        if let Some(why) = matrix_shape_problem(&self.matrix) {
            return Err(ManifestError::BadMatrix(why));
        }
        Ok(())
    }

    /// Values a launch declaration may refer to, resolved at launch time.
    ///
    /// A CLOSED SET, and small. `GOMAXPROCS={NCORE}` is why this exists at all: two manifests set the
    /// Go runtime's thread count from the size of the pinned core range, and a literal there would
    /// mean the gateway runs at the host's core count inside a four-core cpuset - which is the
    /// comparability basis of every number on the board, not a detail. An unknown placeholder is an
    /// error rather than being passed through, because a `{TYPO}` reaching a container as a literal
    /// is a misconfiguration that boots and measures fine.
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
            // own - one template is JSON and is nothing but braces, another documents a URL shape as
            // `{api_base}` in a comment - so both halves have to be escapable: handling only `{{`
            // would render a YAML `error_map: {}` as `{}}`, invalid YAML the gateway refuses to boot.
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
                // A manifest's own declared constant, resolved recursively because one of them is
                // genuinely written in terms of another: a bedrock path built from a bedrock model
                // name. The depth bound is what makes that safe - a constant that refers to itself,
                // directly or in a ring, stops with a named error instead of a stack overflow.
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
    /// else started, which is what every run did until now.
    ///
    /// The container's `--name` is NOT taken from here. It comes from `runtime.identity()`, the same
    /// string the memory readers and the stop path use, so the thing that gets started, the thing
    /// that gets measured and the thing that gets stopped cannot be three different containers.
    ///
    /// `gw_dir` resolves the mounts: a manifest declares its config files relative to its own
    /// directory, because an absolute path in a manifest is a path that only works on one machine.
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

        // THE BUILD STEP, wired to the pre-launch seam that already exists: a declared `build`
        // script must actually run before launch, or a source-built entrant's launcher is told to
        // run a binary nothing ever produced, and it never becomes ready.
        //
        // Run ONCE, before the first attempt, which is where the shell put it and for the same
        // reason: it installs a toolchain and compiles, and that must not happen inside a
        // measurement window.
        let pre_launch = match decl {
            LaunchDecl::Native {
                build: Some(script),
                ..
            } => Some(crate::launch::PreLaunchStep {
                command: gw_dir.join(script).to_string_lossy().into_owned(),
                args: Vec::new(),
                // A release build of a gateway from source, on a cold box that may also be
                // installing a toolchain. Generous on purpose: the failure this bound exists to
                // catch is a hang, and a build that is merely slow is not a hang.
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
                    // THE HARNESS WINS. The gateway's own env goes first and the pinning values
                    // last, because a later assignment overrides an earlier one and no entrant may
                    // opt out of the core limit it is being measured under.
                    env: env
                        .into_iter()
                        .chain(pinned_parallelism(core_count(cores)))
                        .collect(),
                    // Every entrant uses host networking: the gateway binds the port it declares and
                    // the harness drives that port. A published mapping would put a NAT hop inside
                    // every measured request.
                    port: crate::launch::PortMapping::Host,
                    mounts: mounts
                        .iter()
                        .map(|m| crate::launch::Mount {
                            // ABSOLUTE. A container runtime reads a relative source as a named
                            // VOLUME, not a path, and refuses it: "includes invalid characters for a
                            // local volume name". Canonicalize where possible so the path is also
                            // free of `..` and symlinks; fall back to joining if the file is not
                            // there yet, so the failure is the launch reporting a missing config
                            // rather than this silently producing a relative path again.
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
            // `build` is consumed by the pre-launch step, not here.
            LaunchDecl::Native {
                binary,
                args,
                env,
                env_unset,
                ..
            } => {
                // The FIRST declared candidate that exists and is executable. One entrant's crate has
                // no stable output name, so the shell searched three; declaring them keeps the search
                // as data. Falling back to the first candidate when none exists yet is deliberate:
                // before the build has run there is nothing on disk, and `launch` must then fail with
                // its own evidence rather than this returning None and looking like "no launch
                // declared".
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
    /// ALL of it, not the first thing found. Someone adding a gateway wants the list, not a game of
    /// fix-one-rerun. Every finding names the file and says what to do.
    ///
    /// These are the mistakes that otherwise surface as a container that starts and immediately dies,
    /// which reads as the gateway being broken rather than as the setup being wrong.
    pub fn problems(&self, gw_dir: &std::path::Path) -> Vec<String> {
        let mut out = Vec::new();

        if let Err(e) = self.validate() {
            out.push(format!("definition.json: {e}"));
        }

        // A CELL PATH THAT CANNOT SUBSTITUTE MEASURES THE WRONG ROUTE. `cell_paths_for` drops it and
        // the cell silently falls back to the dialect's default path - a different wire than the
        // manifest declared, published under the cell's name. Caught here for the same reason the
        // header and template checks are: before the box-hours are spent.
        for (k, v) in &self.cell_paths {
            if let Err(e) = self.substitute(v, "0-3", 8000, gw_dir) {
                out.push(format!(
                    "cell_paths[{k}]: {v:?} cannot be substituted ({e}), so this cell would measure \
                     the dialect's default route instead of the declared one"
                ));
            }
        }

        // A HEADER LINE WITH NO COLON CANNOT BE SENT.
        //
        // A line missing its colon - "x-api-key bench-token" instead of "x-api-key: bench-token" -
        // has no name/value split. `headers_for` now refuses it at run start; it used to drop it
        // without a word, and the gateway was measured WITHOUT a header its own manifest declared.
        // Caught here too, at validate time, before the box-hours are spent.
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

        // A BUILD SCRIPT THAT IS DECLARED BUT NOT THERE.
        //
        // `validate` checks config templates, not this: a declared build script that does not exist
        // would point the launcher at a binary nothing had built, surfacing on a bench box as
        // `never became ready` instead of here, in a second.
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
            // A CONFIG TEMPLATE IS DATA, AND MUST NOT ASK FOR A COMMAND TO BE RUN.
            //
            // A gateway is a directory of data: static config files, an env block, headers. Nothing
            // executes to produce a config. Templates carried over from the retired shell manifests
            // still contained `$(...)`, which that shell expanded by calling a function; the engine
            // substitutes `{PLACEHOLDER}` and nothing else, so it wrote the text out verbatim and
            // the gateway got a config file with a shell fragment where a provider block belonged.
            //
            // It failed silently in the worst possible way. The gateway booted, bound its port,
            // answered every probe 404, and published as an entrant that serves nothing at all.
            //
            // A TEMPLATE WE COULD NOT READ IS NOT A TEMPLATE THAT PASSED.
            //
            // This was `if let Ok(raw) = ...` with no else arm, so a template that exists (the
            // `is_file` check above passed) but cannot be read - a permissions bit, a transient EIO,
            // an NFS hiccup on CI - skipped BOTH checks below and pushed nothing. `otb validate`
            // reports a gateway as ok when `problems()` comes back empty, so the gate that exists
            // precisely to catch a config with a literal `{MOCK_PORT}` in an upstream URL would pass
            // it, with nothing distinguishing "checked and clean" from "never actually checked".
            // That is the same silent-failure shape the comment above calls the worst possible way to
            // fail, reproduced in the guard against it.
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
                // A placeholder the harness cannot supply. Caught here rather than at launch, where
                // it becomes a gateway booting with a literal `{MOCK_PORT}` in an upstream URL.
                if let Err(e) = self.substitute(&raw, "0-3", 8000, gw_dir) {
                    out.push(format!("{}: {e}", f.template));
                }
            }
        }

        // A mount pointing at a file nothing renders. This is the one that costs the most to
        // diagnose in the wild: the container starts, finds no config, and exits, and the only
        // symptom is a port that never listens.
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

        // The config-necessity standard, reported the same way as everything else rather than as a
        // separate tool a maintainer has to know to run.
        for f in crate::config_lint::lint(self, &Default::default()) {
            out.push(format!("definition.json: {}", f.message));
        }

        out
    }

    /// Read a gateway from its own directory: the definition, plus whatever sidecars it has.
    ///
    /// ONE FILE IS UNIFORM AND THE REST ARE THE GATEWAY'S OWN. `definition.json` has the same shape
    /// for every entrant, so a reader comparing two gateways is comparing like with like. Everything
    /// that differs - the env its process needs, the headers that select its upstreams, the config it
    /// boots on - sits beside it in the form that thing naturally takes.
    ///
    /// Every sidecar is optional. Three entrants are configured entirely by env or headers and have
    /// no config file at all; one has neither and is just a definition.
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
            parse_env(&raw)
                .and_then(|(env, unset)| m.apply_env(env, unset))
                .map_err(|why| ManifestLoadError::Malformed {
                    path: env_path.clone(),
                    why,
                })?;
        }

        // One command per line. Blank lines and `#` comments are skipped so the file can explain
        // itself, which matters more here than anywhere else: these are the only place the harness
        // does something to a gateway rather than handing it a file.
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
    /// Ledger RIG-12's remainder. `run::headers_for` composes the dialect's own credential header
    /// (`Dialect::auth_headers`) and then appends the manifest's `headers` and `egress_headers`
    /// verbatim. Nothing stopped a manifest declaring `authorization` itself, and one does today -
    /// `gateways/litellm-rust/definition.json` line 15, `"Authorization: Bearer {GW_AUTH}"`, which
    /// collides with the bearer header the openai, openai-responses, cohere and bedrock ingress
    /// dialects all send. Both went out on one request, and HTTP does not define which of two
    /// same-named headers a server honours: some take the first, some the last, some join them with
    /// a comma and fail the parse. Nothing errors. The gateway authenticates as SOMEBODY and
    /// publishes a clean number for a request whose credential we cannot state.
    ///
    /// PRECEDENCE, NOT REFUSAL, and the live manifest above is why. Refusing at load is the tidier
    /// rule and it would stop the whole benchmark on a first-party file this change is not allowed
    /// to edit - trading a silently-ambiguous measurement for no measurement at all, on a gateway
    /// whose duplicate happens to be byte-identical to the header it duplicates. So the wire is made
    /// unambiguous instead (`run::headers_for` drops the manifest's copy and keeps the dialect's),
    /// and the collision is DISCLOSED here - printed at the start of every run (`suite.rs`), naming
    /// the file and the header - rather than leaving the rule to be rediscovered from the code. It is
    /// deliberately NOT a `problems()` entry: that would fail `otb validate` on the live manifest.
    ///
    /// The rig's copy wins because the credential is the harness's to assert: `cfg.auth` is the
    /// token this run holds (one gateway mints it at launch), and the header shape is what a real
    /// client of that dialect sends. A manifest that could override it could have the gateway
    /// measured under an identity the harness cannot name, which is the same defect one level up.
    ///
    /// The name list is DERIVED from `Dialect::auth_headers` via `rig_owned_header_names`, over
    /// every dialect, so it cannot go stale when a dialect changes how it authenticates. Every
    /// dialect's, not just the ones this gateway declares: `egress` is a wiring declaration, the
    /// matrix probes all six ingress dialects regardless, and a header in `headers` is sent on every
    /// one of them.
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
    ///
    /// EVERY DECLARED LINE LANDS OR THE LOAD FAILS. This used to drop, silently: the whole sidecar
    /// when no launch is declared, every `-NAME` removal on a docker launch, and any `env` already
    /// declared in definition.json (replaced rather than merged). Each is a variable the gateway's
    /// own directory says it runs with and it did not, so each is refused here with the reason.
    fn apply_env(&mut self, env: Vec<(String, String)>, unset: Vec<String>) -> Result<(), String> {
        match self.launch.as_mut() {
            None if env.is_empty() && unset.is_empty() => Ok(()),
            None => Err(
                "the env file declares variables but definition.json declares no launch, so \
                         nothing would receive them"
                    .to_string(),
            ),
            Some(LaunchDecl::Docker { env: e, .. }) => {
                if !unset.is_empty() {
                    return Err(format!(
                        "-NAME removals ({}) apply only to a native launch: a container does not \
                         inherit the harness environment, so there is nothing to remove",
                        unset.join(", ")
                    ));
                }
                if !e.is_empty() && !env.is_empty() {
                    return Err(
                        "both definition.json (launch.env) and the env file declare the \
                                environment; declare it in one place"
                            .to_string(),
                    );
                }
                if !env.is_empty() {
                    *e = env;
                }
                Ok(())
            }
            Some(LaunchDecl::Native {
                env: e,
                env_unset: u,
                ..
            }) => {
                if (!e.is_empty() && !env.is_empty()) || (!u.is_empty() && !unset.is_empty()) {
                    return Err(
                        "both definition.json (launch.env/env_unset) and the env file \
                                declare the environment; declare it in one place"
                            .to_string(),
                    );
                }
                if !env.is_empty() {
                    *e = env;
                }
                if !unset.is_empty() {
                    *u = unset;
                }
                Ok(())
            }
        }
    }

    /// `cell_paths` with its placeholders resolved, the same way `headers_for` resolves theirs.
    ///
    /// A per-cell path is exactly where a placeholder earns its keep. Bedrock's standard path embeds
    /// the model, and a gateway's Bedrock model id is never its OpenAI one, so the cell's real route
    /// is built from a constant the manifest already declares for its config template. Cloning these
    /// raw would send a literal `{NAME}` to the gateway, which answers 404, and a 404 on a declared
    /// pairing is published as the gateway not serving it - a red it did not earn, caused by us.
    ///
    /// Resolving here rather than pinning the expanded string in `cell_paths` keeps ONE source for
    /// the value: pinning it would go stale silently the day the constant changes, reintroducing the
    /// same unearned red with nothing to signal it.
    ///
    /// A key that cannot be resolved is dropped rather than sent half-substituted, so `path_for`
    /// falls back to the dialect's standard path - a wrong-but-honest probe of a real endpoint,
    /// never a request for a path built out of our own template syntax.
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
                    // A CELL PATH THAT WOULD NOT SUBSTITUTE USED TO VANISH HERE.
                    //
                    // `.ok()?` dropped it silently, so a cell_path carrying a placeholder the harness
                    // cannot supply left no entry and no word - and the cell then measured the
                    // DIALECT'S DEFAULT path instead. A different route than the manifest declared,
                    // published under the cell's name, which is the one thing this field exists to
                    // prevent.
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
    /// `authorization` is added by the caller, not here, because it is the same for every column and
    /// one gateway mints it at launch rather than declaring it.
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
            // A manifest writes headers the way they appear on the wire, "Name: value", because that
            // is how they are read in the gateway's own docs and how they were declared in shell.
            // A line with no colon used to be dropped without a word, and the gateway was measured
            // without a header its manifest declared. The run refuses it instead, like any other
            // header that cannot resolve.
            let Some((name, value)) = resolved.split_once(':') else {
                return Err(ManifestError::HeaderWithoutColon(line.clone()));
            };
            out.push((name.trim().to_string(), value.trim().to_string()));
        }
        Ok(out)
    }

    /// Render every declared config file into the gateway's directory.
    ///
    /// Returns the paths written, so a caller can publish them as the artifact's config record: the
    /// bytes a gateway booted with belong in the same artifact as the numbers they produced, which is
    /// what stops a chart being read against a config that was overwritten later.
    ///
    /// A template that refers to something the harness does not supply is an ERROR, not a passthrough.
    /// A gateway booting with a literal `{MOCK_PORT}` in its upstream URL fails in a way that looks
    /// like the gateway being broken.
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

// The shared test `Manifest` (`test_fixture`) lives in a side file; re-exported here so callers
// keep using `crate::manifest::test_fixture`.
#[cfg(test)]
#[path = "tests/manifest_fixture.rs"]
mod fixture;
#[cfg(test)]
pub(crate) use fixture::test_fixture;

#[cfg(test)]
#[path = "tests/manifest_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "tests/manifest_real_field_tests.rs"]
mod real_field_tests;

#[cfg(test)]
#[path = "tests/manifest_unresolvable_header_tests.rs"]
mod unresolvable_header_tests;

#[cfg(test)]
#[path = "tests/manifest_unknown_field_tests.rs"]
mod unknown_field_tests;
