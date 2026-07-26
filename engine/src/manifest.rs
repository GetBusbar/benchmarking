// SPDX-License-Identifier: Apache-2.0
// Copyright (C) 2026 Busbar Inc and contributors
//
// A gateway manifest, as DATA.
//
// The shell manifests express identity as code: each one hand-writes `gw_rss`, `gw_hwm` and
// `gw_stop`, thirteen times over, and every trio has to name the same container or the same process
// pattern. The harness comment says the readers are "matched BY CONSTRUCTION, not by convention",
// and at the library level that is true, but the manifest still spells the name out once per hook,
// so the matching is a convention again at exactly the layer a human edits.
//
// That drift has already corrupted published numbers. Three source-built manifests wrote a
// single-pid reader for RSS beside a whole-tree reader for HWM, so for the same gateway idle, peak
// and recovered measured ONE process while the high-water mark measured that process and every
// descendant. Two different populations, published side by side, and compared against gateways whose
// readers were tree-summed. A gateway that forks workers had its peak inflated relative to its idle
// by whatever its children weighed.
//
// Here identity is declared ONCE. Every reader derives from it, so RSS and HWM cannot describe
// different populations, and a stop cannot target something the readers never measured. The class of
// bug is removed rather than guarded.

use serde::{Deserialize, Serialize};

/// How the gateway runs, and therefore how its process tree is found. This is the single declaration
/// every memory reader and the stop path derive from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Runtime {
    /// A container. The root pid comes from the container runtime, and the tree walk starts there.
    Docker { container: String },
    /// A process started directly on the box, located by a match against its command line.
    Native { proc_match: String },
}

impl Runtime {
    /// The one identity string, whatever the kind. Readers take this rather than being handed a name
    /// per call site, which is what makes a mismatch between them unrepresentable.
    pub fn identity(&self) -> &str {
        match self {
            Runtime::Docker { container } => container,
            Runtime::Native { proc_match } => proc_match,
        }
    }

    pub fn is_docker(&self) -> bool {
        matches!(self, Runtime::Docker { .. })
    }
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigSetting {
    pub key: String,
    pub reason: ConfigReason,
    /// Free text for a human, never load bearing.
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    pub auth: String,
    #[serde(default)]
    pub headers: Vec<String>,
    pub runtime: Runtime,
    /// Egress dialects the manifest configures. NOT a capability claim: the matrix probes every cell
    /// regardless and publishes what it observes. This only says which upstreams are wired.
    #[serde(default)]
    pub egress: Vec<String>,
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
    /// `lib/gateway_isolation_test.sh` exempts files under `gateways/<name>/` from Rule 1 but scans
    /// `.rs` everywhere, so a Rust function rendering a config that must contain the gateway's own
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

/// How many cores a cpuset string covers, e.g. "0-3" is four.
///
/// This is the same arithmetic the shell manifests do inline, and it is the value the Go runtime is
/// pinned to. A single core ("2") is one; anything unparseable is one, because claiming more
/// parallelism than the pin allows is the direction that corrupts a measurement.
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
        /// attempt of every column: thirty-six cells lost to a variable NAME, once already.
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
    Unreadable { path: std::path::PathBuf, why: String },
    Malformed { path: std::path::PathBuf, why: String },
}

impl std::fmt::Display for ManifestLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ManifestLoadError::Unreadable { path, why } => write!(f, "cannot read {}: {why}", path.display()),
            ManifestLoadError::Malformed { path, why } => write!(f, "{} is not valid: {why}", path.display()),
        }
    }
}

impl std::error::Error for ManifestLoadError {}

/// Parse a sidecar env file: `KEY=value` sets, a leading `-` REMOVES.
///
/// Removal is not a convenience. One entrant's config loader claims every variable sharing its
/// prefix and feeds it to a deny-unknown-fields deserializer, so the harness's own override
/// variables kill config load before the port binds - and the process is backgrounded, so the launch
/// reports success and the only symptom is a port that never listens. That cost thirty-six cells
/// once. An env block that can only ADD cannot express it.
///
/// Deliberately parsed, never executed: this file used to be shell, and the whole point of the
/// rewrite is that nothing in the measurement path is.
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
    Unreadable { path: std::path::PathBuf, why: String },
    Unwritable { path: std::path::PathBuf, why: String },
    Placeholder { path: std::path::PathBuf, why: String },
}

impl std::fmt::Display for ConfigRenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigRenderError::Unreadable { path, why } => write!(f, "cannot read config template {}: {why}", path.display()),
            ConfigRenderError::Unwritable { path, why } => write!(f, "cannot write rendered config {}: {why}", path.display()),
            ConfigRenderError::Placeholder { path, why } => write!(f, "config template {}: {why}", path.display()),
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
    UnexpandedVariable { field: &'static str, raw: String },
    /// A config setting with no stated necessity cannot be lint-checked, so it cannot ship.
    ConfigWithoutReason(String),
    /// A launch declaration refers to something the harness does not supply. Loud rather than passed
    /// through: a `{TYPO}` reaching a container as a literal is a misconfiguration that boots and
    /// measures fine, and publishes a number taken under the wrong settings.
    UnknownPlaceholder { name: String, raw: String },
    /// A declared constant refers to itself, directly or through a ring of others.
    ConstantCycle { name: String },
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
            ManifestError::ConstantCycle { name } => {
                write!(f, "constant {name:?} refers to itself, directly or through a ring of others")
            }
            ManifestError::UnknownPlaceholder { name, raw } => write!(
                f,
                "launch declaration refers to {{{name}}}, which the harness does not supply, in {raw:?}"
            ),
            ManifestError::UnexpandedVariable { field, raw } => write!(
                f,
                "{field} still holds an unexpanded shell variable ({raw:?}): the extraction took the reference, not the value"
            ),
        }
    }
}

impl Manifest {
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
                return Err(ManifestError::UnexpandedVariable { field, raw: v.clone() });
            }
        }
        for h in &self.headers {
            if h.contains('$') {
                return Err(ManifestError::UnexpandedVariable { field: "headers", raw: h.clone() });
            }
        }
        if self.runtime.identity().contains('$') {
            return Err(ManifestError::UnexpandedVariable {
                field: "runtime identity",
                raw: self.runtime.identity().to_string(),
            });
        }
        if self.runtime.identity().trim().is_empty() {
            return Err(ManifestError::Empty("runtime identity"));
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
            // `{api_base}` in a comment - so both halves have to be escapable. Handling only `{{`
            // rendered `error_map: {}` as `{}}` and the gateway refused to boot on invalid YAML.
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
                        return Err(ManifestError::ConstantCycle { name: name.to_string() });
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
    /// that gets measured and the thing that gets stopped cannot be three different containers. That
    /// is the defect this module's header describes as having already corrupted published numbers.
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

        let kind = match decl {
            LaunchDecl::Docker { image, env, args, mounts } => {
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
                    env,
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
            LaunchDecl::Native { build, binary, args, env, env_unset } => {
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
                let _ = build; // consumed by the pre-launch step, wired separately
                crate::launch::LaunchKind::Native {
                    binary: bin.to_string_lossy().into_owned(),
                    args,
                    env,
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
            pre_launch: None,
        }))
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
        let text = std::fs::read_to_string(&def_path)
            .map_err(|e| ManifestLoadError::Unreadable { path: def_path.clone(), why: e.to_string() })?;
        let mut m: Manifest = serde_json::from_str(&text)
            .map_err(|e| ManifestLoadError::Malformed { path: def_path.clone(), why: e.to_string() })?;

        let env_path = dir.join("env");
        if env_path.is_file() {
            let raw = std::fs::read_to_string(&env_path)
                .map_err(|e| ManifestLoadError::Unreadable { path: env_path.clone(), why: e.to_string() })?;
            let (env, unset) = parse_env(&raw);
            m.apply_env(env, unset);
        }

        let headers_path = dir.join("headers.json");
        if headers_path.is_file() {
            let raw = std::fs::read_to_string(&headers_path)
                .map_err(|e| ManifestLoadError::Unreadable { path: headers_path.clone(), why: e.to_string() })?;
            m.egress_headers = serde_json::from_str(&raw)
                .map_err(|e| ManifestLoadError::Malformed { path: headers_path.clone(), why: e.to_string() })?;
        }
        Ok(m)
    }

    /// Put a sidecar's env onto whichever launch kind this manifest declares.
    fn apply_env(&mut self, env: Vec<(String, String)>, unset: Vec<String>) {
        match self.launch.as_mut() {
            Some(LaunchDecl::Docker { env: e, .. }) => *e = env,
            Some(LaunchDecl::Native { env: e, env_unset: u, .. }) => {
                *e = env;
                *u = unset;
            }
            None => {}
        }
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
        let lines = self.headers.iter().chain(self.egress_headers.get(egress).into_iter().flatten());
        for line in lines {
            let resolved = self.substitute(line, cores, mock_port, gw_dir)?;
            // A manifest writes headers the way they appear on the wire, "Name: value", because that
            // is how they are read in the gateway's own docs and how they were declared in shell.
            if let Some((name, value)) = resolved.split_once(':') {
                out.push((name.trim().to_string(), value.trim().to_string()));
            }
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
            let raw = std::fs::read_to_string(&template_path)
                .map_err(|e| ConfigRenderError::Unreadable { path: template_path.clone(), why: e.to_string() })?;
            let body = self
                .substitute(&raw, cores, mock_port, gw_dir)
                .map_err(|e| ConfigRenderError::Placeholder { path: template_path.clone(), why: e.to_string() })?;
            let out_path = gw_dir.join(&file.output);
            if let Some(parent) = out_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| ConfigRenderError::Unwritable { path: out_path.clone(), why: e.to_string() })?;
            }
            std::fs::write(&out_path, &body)
                .map_err(|e| ConfigRenderError::Unwritable { path: out_path.clone(), why: e.to_string() })?;
            written.push((out_path, body));
        }
        Ok(written)
    }

    /// The URL the harness drives this gateway on.
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}{}", self.port, self.path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docker_manifest() -> Manifest {
        Manifest {
            name: "gw".into(),
            display: "GW".into(),
            lang: "Rust".into(),
            class: "AI gateway".into(),
            repo: "https://example.invalid/gw".into(),
            port: 8080,
            path: "/v1/chat/completions".into(),
            model: "m".into(),
            auth: "dummy".into(),
            headers: vec![],
            runtime: Runtime::Docker { container: "gw-bench".into() },
            egress: vec!["openai".into()],
            config: vec![],
            launch: None,
            config_files: vec![],
            constants: Default::default(),
            egress_headers: Default::default(),
        }
    }

    // THE WHOLE POINT. RSS, HWM and stop all read ONE declaration, so they cannot name different
    // things. In shell each was a separate hand-written hook, and three manifests did in fact drift:
    // a single-pid reader for RSS beside a whole-tree reader for HWM, publishing two different
    // populations for the same gateway.
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
        let m = Manifest { runtime: Runtime::Native { proc_match: "target/release/gw".into() }, ..docker_manifest() };
        assert!(!m.runtime.is_docker());
        assert_eq!(m.runtime.identity(), "target/release/gw");
    }

    // A runtime with no identity cannot be measured or stopped, so it must not validate.
    #[test]
    fn an_empty_runtime_identity_is_rejected() {
        let m = Manifest { runtime: Runtime::Docker { container: "  ".into() }, ..docker_manifest() };
        assert_eq!(m.validate(), Err(ManifestError::Empty("runtime identity")));
    }

    #[test]
    fn required_fields_are_required() {
        for (mutate, field) in [
            (Box::new(|m: &mut Manifest| m.name.clear()) as Box<dyn Fn(&mut Manifest)>, "name"),
            (Box::new(|m: &mut Manifest| m.display.clear()), "display"),
            (Box::new(|m: &mut Manifest| m.repo.clear()), "repo"),
            (Box::new(|m: &mut Manifest| m.path.clear()), "path"),
            (Box::new(|m: &mut Manifest| m.model.clear()), "model"),
        ] {
            let mut m = docker_manifest();
            mutate(&mut m);
            assert_eq!(m.validate(), Err(ManifestError::Empty(field)), "{field} must be required");
        }
        let mut m = docker_manifest();
        m.port = 0;
        assert_eq!(m.validate(), Err(ManifestError::BadPort));
    }

    // Every declared setting must name which of the four necessities it satisfies. As shell this was
    // free text a lint grepped; here a setting cannot be constructed without one, and there is no
    // variant meaning "we wanted this feature on".
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

    // The shell manifests are one indirection deep: one gateway sets `SOME_MODEL=gpt-4o-mini` and
    // then `GW_MODEL="$SOME_MODEL"`. Extracting the reference instead of the value yields a field
    // that is non-empty, parses, and validates under every other rule, then goes out on the wire as a
    // model name. The corpus shipped exactly this.
    #[test]
    fn a_field_holding_an_unexpanded_shell_variable_is_rejected() {
        let m = Manifest { model: "$SOME_MODEL".into(), ..docker_manifest() };
        assert_eq!(
            m.validate(),
            Err(ManifestError::UnexpandedVariable { field: "model", raw: "$SOME_MODEL".into() })
        );
        assert!(!m.model.trim().is_empty(), "the point: it is non-empty, so the emptiness checks pass it");

        // Every field a request or a launch is built from, not just the model.
        let m = Manifest { auth: "${GW_KEY}".into(), ..docker_manifest() };
        assert!(matches!(m.validate(), Err(ManifestError::UnexpandedVariable { field: "auth", .. })));
        let m = Manifest { headers: vec!["x-api-key: $GW_AUTH".into()], ..docker_manifest() };
        assert!(matches!(m.validate(), Err(ManifestError::UnexpandedVariable { field: "headers", .. })));
        let m = Manifest { runtime: Runtime::Docker { container: "$NAME-bench".into() }, ..docker_manifest() };
        assert!(matches!(m.validate(), Err(ManifestError::UnexpandedVariable { field: "runtime identity", .. })));

        // A clean manifest is untouched by the new rule.
        assert!(docker_manifest().validate().is_ok());
    }

    #[test]
    fn a_setting_with_no_key_cannot_ship() {
        let m = Manifest {
            config: vec![ConfigSetting { key: " ".into(), reason: ConfigReason::RequiredToBoot, note: String::new() }],
            ..docker_manifest()
        };
        assert!(matches!(m.validate(), Err(ManifestError::ConfigWithoutReason(_))));
    }

    #[test]
    fn round_trips_through_json_including_the_runtime_tag() {
        let m = docker_manifest();
        let js = serde_json::to_string(&m).unwrap();
        assert!(js.contains(r#""kind":"docker""#), "the runtime kind must be explicit on the wire: {js}");
        let back: Manifest = serde_json::from_str(&js).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn the_url_is_built_from_the_declared_port_and_path() {
        assert_eq!(docker_manifest().url(), "http://127.0.0.1:8080/v1/chat/completions");
    }
}

#[cfg(test)]
mod real_field_tests {
    use super::*;
    use std::collections::BTreeMap;

    /// Every manifest in the real field, read from the gateways' own directories.
    ///
    /// DISCOVERED, not listed. A single file naming all thirteen is the hand-maintained roster
    /// `lib/gateway_isolation_test.sh` exists to prevent, and it was invisible to that lint only
    /// because the scan skips `.json`. Reading the directory means adding a gateway is dropping in a
    /// directory, and nothing else in the tree learns its name.
    ///
    /// A schema that only represents an example I invented proves nothing: if the types cannot
    /// describe all thirteen entrants as they actually are, the schema is wrong and no amount of
    /// internal consistency would say so.
    fn field() -> BTreeMap<String, Manifest> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../gateways");
        let mut out = BTreeMap::new();
        for entry in std::fs::read_dir(&root).expect("the gateways directory must exist").flatten() {
            let def = entry.path().join("definition.json");
            if !def.is_file() {
                continue;
            }
            let m = Manifest::load(&entry.path())
                .unwrap_or_else(|e| panic!("{e}"));
            out.insert(m.name.clone(), m);
        }
        assert!(!out.is_empty(), "no gateways/*/definition.json found");
        out
    }

    #[test]
    fn every_real_manifest_parses_and_validates() {
        let f = field();
        assert!(f.len() >= 13, "the whole field should be represented, got {}", f.len());
        for (name, m) in &f {
            assert!(m.validate().is_ok(), "{name} must validate: {:?}", m.validate());
            assert_eq!(&m.name, name, "the key and the declared name must agree");
        }
    }

    /// The regression this schema exists to make impossible. Today all thirteen agree, because the
    /// shell defect was found and fixed by hand; the point is that after this there is no second
    /// place to spell the identity, so they cannot drift apart again.
    #[test]
    fn no_manifest_can_name_two_different_things_to_measure() {
        for (name, m) in &field() {
            let id = m.runtime.identity();
            assert!(!id.trim().is_empty(), "{name} must declare something measurable");
            // Both readers and the stop path take this one string. Asserting it twice is the closest
            // a test can get to asserting that a second spelling does not exist.
            assert_eq!(m.runtime.identity(), id);
        }
    }

    /// THE THING THAT WAS MISSING: every container manifest can now produce a real invocation.
    ///
    /// `launch.rs` was complete and tested and had zero callers, because a `Manifest` carried no
    /// launch data and nothing bridged the two. This walks the real corpus and builds the actual
    /// docker command line for each one.
    #[test]
    fn every_container_manifest_produces_a_launchable_invocation() {
        use std::time::Duration;
        let mut launchable = 0;
        for (name, m) in &field() {
            let spec = m
                .launch_spec("0-3", 8000, std::path::Path::new("/gw"), Duration::from_secs(1), Duration::from_secs(1))
                .unwrap_or_else(|| panic!("{name} must declare how it is launched"))
                .unwrap_or_else(|e| panic!("{name} must produce a launchable spec: {e}"));
            assert!(spec.validate().is_ok(), "{name}: {:?}", spec.validate());

            let inv = crate::launch::build_invocation(&spec);
            // A container is started by the container runtime; a source-built entrant is started
            // pinned, directly. Both are launchable, which is the thing that was missing.
            let expected = if m.runtime.is_docker() { "docker" } else { "taskset" };
            assert_eq!(inv.program, expected, "{name}");
            if !m.runtime.is_docker() {
                launchable += 1;
                continue;
            }
            // The container name comes from runtime.identity(), NOT from the launch block, so the
            // thing started, the thing measured and the thing stopped cannot be three containers.
            assert!(
                inv.args.windows(2).any(|w| w == ["--name".to_string(), m.runtime.identity().to_string()]),
                "{name} must launch under its declared identity: {:?}",
                inv.args
            );
            assert!(
                inv.args.windows(2).any(|w| w == ["--cpuset-cpus".to_string(), "0-3".to_string()]),
                "{name} must be pinned: {:?}",
                inv.args
            );
            // A mount is resolved against the gateway's own directory: an absolute path in a manifest
            // only works on the machine it was written on.
            for a in &inv.args {
                assert!(!a.contains("$GW_DIR"), "{name} left an unexpanded shell path: {a}");
                assert!(!a.contains('{') || !a.contains('}'), "{name} left an unresolved placeholder: {a}");
            }
            launchable += 1;
        }
        assert_eq!(launchable, 13, "every entrant must be launchable, got {launchable}");
    }

    /// The Go runtime's thread count is set from the size of the pinned core range. A literal there
    /// would run the gateway at the host's core count inside a four-core cpuset, which is not a
    /// detail: the core split IS the comparability basis of every number on the board.
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
        assert!(!with_ncore.is_empty(), "some entrants set their thread count from the core pin");

        for (name, m) in with_ncore {
            for (cores, expected) in [("0-3", "4"), ("4-9", "6"), ("2", "1")] {
                let spec = m
                    .launch_spec(cores, 8000, std::path::Path::new("/gw"), Duration::from_secs(1), Duration::from_secs(1))
                    .and_then(Result::ok)
                    .unwrap_or_else(|| panic!("{name} must build a spec"));
                let crate::launch::LaunchKind::Docker { env, .. } = &spec.kind else {
                    panic!("{name} is a container entrant")
                };
                let v = env.iter().find(|(k, _)| k == "GOMAXPROCS").map(|(_, v)| v.as_str());
                assert_eq!(v, Some(expected), "{name} on cores {cores} must run {expected} threads");
            }
        }
    }

    /// THE THIRTY-SIX CELL DEFECT, as a test.
    ///
    /// One entrant's config loader claims every environment variable sharing its prefix and feeds it
    /// to a deny-unknown-fields deserializer, so the harness's OWN documented override variables kill
    /// config load before the port binds. The binary is backgrounded, so the launch still reports
    /// success and the only symptom is "port not listening" on every attempt of every column.
    ///
    /// An env block can only ADD, and a spawned process inherits its parent's environment, so this
    /// has to be expressible as a removal or the class cannot be prevented at all.
    #[test]
    fn a_native_entrant_can_require_that_a_variable_is_absent_not_merely_unset_by_us() {
        use std::time::Duration;
        let f = field();
        let scrubbing: Vec<_> = f
            .iter()
            .filter(|(_, m)| {
                m.launch.as_ref().is_some_and(|l| matches!(l, LaunchDecl::Native { env_unset, .. } if !env_unset.is_empty()))
            })
            .collect();
        assert!(
            !scrubbing.is_empty(),
            "at least one entrant must scrub the environment it is launched with; that requirement is why this field exists"
        );

        for (name, m) in scrubbing {
            let spec = m
                .launch_spec("0-3", 8000, std::path::Path::new("/gw"), Duration::from_secs(1), Duration::from_secs(1))
                .and_then(Result::ok)
                .unwrap_or_else(|| panic!("{name} must build a spec"));
            let inv = crate::launch::build_invocation(&spec);
            assert!(
                !inv.env_unset.is_empty(),
                "{name} declares variables that must not reach it, and the invocation must carry that removal: {inv:?}"
            );
            // The removals must not merely be set to empty: an empty value is still a present
            // variable, and a loader that rejects unknown KEYS does not care what the value is.
            for removed in &inv.env_unset {
                assert!(
                    !inv.env.iter().any(|(k, _)| k == removed),
                    "{name} both sets and removes {removed}; setting it to anything still leaves it present"
                );
            }
        }
    }

    /// A doubled brace is a literal one, BOTH ways round. Handling only the opening half rendered a
    /// YAML `error_map: {}` as `{}}`, and the gateway refused to boot on invalid YAML - found by
    /// running it, not by reading it.
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
        // Built here rather than borrowed from the other test module: this one walks the real
        // corpus, and a fixture that drifts from the real shape would prove nothing about it.
        let mut m = field().values().find(|m| m.runtime.is_docker()).cloned().expect("a container entrant");
        m.launch = Some(LaunchDecl::Docker {
            image: "gw:1".into(),
            env: vec![("X".into(), "{NOT_A_THING}".into())],
            args: vec![],
            mounts: vec![],
        });
        let err = m
            .launch_spec("0-3", 8000, std::path::Path::new("/gw"), Duration::from_secs(1), Duration::from_secs(1))
            .and_then(Result::err);
        assert!(
            matches!(err, Some(ManifestError::UnknownPlaceholder { ref name, .. }) if name == "NOT_A_THING"),
            "an unknown placeholder must be refused, not passed through as a literal: {err:?}"
        );
    }

    /// EVERY DECLARED CONFIG TEMPLATE ACTUALLY RENDERS.
    ///
    /// Ten entrants boot from a file the harness writes. A template that refers to something the
    /// harness does not supply produces a gateway that starts and immediately dies, which reads as
    /// the gateway being broken - so this fails here, at the manifest, rather than there.
    #[test]
    fn every_declared_config_template_renders_with_nothing_left_unresolved() {
        let mut rendered = 0;
        for (name, m) in &field() {
            if m.config_files.is_empty() {
                continue;
            }
            let gw_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../gateways").join(name);
            let out = std::env::temp_dir().join(format!("otb-render-{}-{}", name, std::process::id()));
            std::fs::create_dir_all(&out).expect("scratch dir");

            for file in &m.config_files {
                let template_path = gw_dir.join(&file.template);
                let raw = std::fs::read_to_string(&template_path)
                    .unwrap_or_else(|e| panic!("{name} declares {} which cannot be read: {e}", file.template));
                let body = m
                    .substitute(&raw, "0-3", 8000, &gw_dir)
                    .unwrap_or_else(|e| panic!("{name} template {}: {e}", file.template));

                // The output is deliberately NOT scanned for leftover braces. A rendered literal -
                // a config format's own syntax, or a comment documenting a URL shape as
                // `{api_base}` - is written `{{...}}` in the template and comes out as `{...}`,
                // which is indistinguishable from an unresolved placeholder by looking at the
                // result. The guarantee lives in `substitute`, which refuses an unknown name
                // outright, so reaching this line means every placeholder was supplied.
                assert!(!body.trim().is_empty(), "{name} rendered {} to nothing", file.output);
                rendered += 1;
            }
            let _ = std::fs::remove_dir_all(&out);
        }
        assert!(rendered >= 13, "every declared template must render, got {rendered}");
    }

    #[test]
    fn both_runtime_kinds_are_present_in_the_real_field() {
        let f = field();
        assert!(f.values().any(|m| m.runtime.is_docker()), "some entrants run in containers");
        assert!(f.values().any(|m| !m.runtime.is_docker()), "some entrants run natively from source");
    }

    #[test]
    fn every_manifest_declares_a_reachable_url() {
        for (name, m) in &field() {
            let u = m.url();
            assert!(u.starts_with("http://127.0.0.1:"), "{name} must be driven on loopback, got {u}");
            assert!(u.contains(&m.port.to_string()));
        }
    }
}
